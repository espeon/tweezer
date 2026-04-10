use std::{
    collections::HashMap,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::JoinSet;

use crate::{
    Adapter, Context, Event, TweezerError,
    trigger::{TriggerContext, TriggerKind},
    typemap::TypeMap,
};

type BoxFuture = Pin<Box<dyn Future<Output = Result<(), TweezerError>> + Send + 'static>>;
type Handler = Arc<dyn Fn(Context) -> BoxFuture + Send + Sync>;
type TriggerHandler = Arc<dyn Fn(TriggerContext) -> BoxFuture + Send + Sync>;
type BeforeCommandHook = Arc<dyn Fn(Context, String) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;
type AfterCommandHook = Arc<dyn Fn(Context, String, Result<(), TweezerError>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;
type UnrecognizedCommandHook = Arc<dyn Fn(Context, String) -> BoxFuture + Send + Sync>;

struct CooldownState {
    global_cooldown: Option<Duration>,
    user_cooldown: Option<Duration>,
    last_global: Arc<std::sync::RwLock<Option<Instant>>>,
    last_per_user: Arc<std::sync::RwLock<HashMap<String, Instant>>>,
}

impl CooldownState {
    fn new(global: Option<Duration>, user: Option<Duration>) -> Self {
        Self {
            global_cooldown: global,
            user_cooldown: user,
            last_global: Arc::new(std::sync::RwLock::new(None)),
            last_per_user: Arc::new(std::sync::RwLock::new(HashMap::new())),
        }
    }

    fn check_and_mark(&self, user_id: &str) -> bool {
        let now = Instant::now();

        if let Some(dur) = self.global_cooldown {
            let lock = self.last_global.read().unwrap();
            if let Some(last) = *lock {
                if now.duration_since(last) < dur {
                    return false;
                }
            }
        }

        if let Some(dur) = self.user_cooldown {
            let lock = self.last_per_user.read().unwrap();
            if let Some(last) = lock.get(user_id) {
                if now.duration_since(*last) < dur {
                    return false;
                }
            }
        }

        if self.global_cooldown.is_some() {
            *self.last_global.write().unwrap() = Some(now);
        }
        if self.user_cooldown.is_some() {
            self.last_per_user.write().unwrap().insert(user_id.to_string(), now);
        }

        true
    }
}

struct CommandEntry {
    handler: Handler,
    description: Option<String>,
    category: Option<String>,
    #[allow(dead_code)]
    platform: Option<String>,
    cooldown: Arc<CooldownState>,
}

#[derive(Clone)]
pub struct HelpEntry {
    pub name: String,
    pub description: Option<String>,
    pub category: Option<String>,
}

pub struct Command<F, Fut> {
    name: String,
    handler: F,
    description: Option<String>,
    category: Option<String>,
    channel: Option<String>,
    platform: Option<String>,
    cooldown: Option<Duration>,
    user_cooldown: Option<Duration>,
    _phantom: PhantomData<Fut>,
}

impl<F, Fut> Command<F, Fut>
where
    F: Fn(Context) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), TweezerError>> + Send + 'static,
{
    pub fn new(name: impl Into<String>, handler: F) -> Self {
        Self {
            name: name.into(),
            handler,
            description: None,
            category: None,
            channel: None,
            platform: None,
            cooldown: None,
            user_cooldown: None,
            _phantom: PhantomData,
        }
    }

    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    pub fn category(mut self, cat: impl Into<String>) -> Self {
        self.category = Some(cat.into());
        self
    }

    pub fn channel(mut self, ch: impl Into<String>) -> Self {
        self.channel = Some(ch.into());
        self
    }

    pub fn platform(mut self, p: impl Into<String>) -> Self {
        self.platform = Some(p.into());
        self
    }

    pub fn cooldown(mut self, duration: Duration) -> Self {
        self.cooldown = Some(duration);
        self
    }

    pub fn user_cooldown(mut self, duration: Duration) -> Self {
        self.user_cooldown = Some(duration);
        self
    }
}

// ---------------------------------------------------------------------------
// Error hook
// ---------------------------------------------------------------------------

pub struct HandlerError {
    pub kind: HandlerErrorKind,
    pub platform: String,
    pub channel: String,
    pub error: TweezerError,
}

#[derive(Debug)]
pub enum HandlerErrorKind {
    Command { name: String },
    RawMessage,
    Trigger,
    UnrecognizedCommand,
}

type ErrorHook = Arc<dyn Fn(HandlerError) + Send + Sync>;

fn default_error_handler(e: HandlerError) {
    match e.kind {
        HandlerErrorKind::Command { name } => {
            eprintln!(
                "[{}/{}] command '{}' error: {}",
                e.platform, e.channel, name, e.error
            )
        }
        HandlerErrorKind::RawMessage => {
            eprintln!(
                "[{}/{}] raw message handler error: {}",
                e.platform, e.channel, e.error
            )
        }
        HandlerErrorKind::Trigger => {
            eprintln!(
                "[{}/{}] trigger handler error: {}",
                e.platform, e.channel, e.error
            )
        }
        HandlerErrorKind::UnrecognizedCommand => {
            eprintln!(
                "[{}/{}] unrecognized command handler error: {}",
                e.platform, e.channel, e.error
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Trigger matcher
// ---------------------------------------------------------------------------

enum TriggerMatcher {
    Any,
    AnyRaid,
    AnyFollow,
    AnySubscription,
    AnyDonation,
    PlatformKind(String),
}

impl TriggerMatcher {
    fn matches(&self, kind: &TriggerKind) -> bool {
        match self {
            TriggerMatcher::Any => true,
            TriggerMatcher::AnyRaid => matches!(kind, TriggerKind::Raid { .. }),
            TriggerMatcher::AnyFollow => matches!(kind, TriggerKind::Follow { .. }),
            TriggerMatcher::AnySubscription => matches!(kind, TriggerKind::Subscription { .. }),
            TriggerMatcher::AnyDonation => matches!(kind, TriggerKind::Donation { .. }),
            TriggerMatcher::PlatformKind(id) => {
                matches!(kind, TriggerKind::Platform(t) if t.kind_id() == id)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Shutdown
// ---------------------------------------------------------------------------

pub struct ShutdownHandle {
    tx: watch::Sender<bool>,
}

impl ShutdownHandle {
    pub fn shutdown(&self) {
        let _ = self.tx.send(true);
    }
}

impl Clone for ShutdownHandle {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Bot
// ---------------------------------------------------------------------------

pub struct Bot {
    adapters: Vec<Box<dyn Adapter>>,
    command_prefix: char,
    commands: HashMap<String, CommandEntry>,
    channel_commands: HashMap<(String, String), CommandEntry>,
    platform_commands: HashMap<(String, String), CommandEntry>,
    raw_message_handlers: Vec<Handler>,
    trigger_handlers: Vec<(TriggerMatcher, TriggerHandler)>,
    error_hook: Option<ErrorHook>,
    before_command_hook: Option<BeforeCommandHook>,
    after_command_hook: Option<AfterCommandHook>,
    unrecognized_command_hook: Option<UnrecognizedCommandHook>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    max_concurrency: usize,
    started_at: Instant,
    state: TypeMap,
}

impl Bot {
    pub fn new() -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            adapters: Vec::new(),
            command_prefix: '!',
            commands: HashMap::new(),
            channel_commands: HashMap::new(),
            platform_commands: HashMap::new(),
            raw_message_handlers: Vec::new(),
            trigger_handlers: Vec::new(),
            error_hook: None,
            before_command_hook: None,
            after_command_hook: None,
            unrecognized_command_hook: None,
            shutdown_tx,
            shutdown_rx,
            max_concurrency: 256,
            started_at: Instant::now(),
            state: TypeMap::new(),
        }
    }

    /// Look up a value previously registered with `Bot::register`.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.state.get::<T>()
    }

    /// Register a value that will be accessible in every handler via `ctx.state::<T>()`.
    /// Registering the same type twice overwrites the previous value.
    pub fn register<T: Send + Sync + 'static>(&mut self, value: T) {
        self.state.insert(value);
    }

    pub fn command_prefix(mut self, prefix: char) -> Self {
        self.command_prefix = prefix;
        self
    }

    pub fn max_concurrency(mut self, n: usize) -> Self {
        self.max_concurrency = n;
        self
    }

    pub fn shutdown_handle(&self) -> ShutdownHandle {
        ShutdownHandle {
            tx: self.shutdown_tx.clone(),
        }
    }

    pub fn add_adapter(&mut self, adapter: impl Adapter + 'static) {
        self.adapters.push(Box::new(adapter));
    }

    pub fn on_error<F: Fn(HandlerError) + Send + Sync + 'static>(&mut self, f: F) {
        self.error_hook = Some(Arc::new(f));
    }

    pub fn on_before_command<F, Fut>(&mut self, hook: F)
    where
        F: Fn(Context, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = bool> + Send + 'static,
    {
        self.before_command_hook = Some(Arc::new(move |ctx, cmd| Box::pin(hook(ctx, cmd))));
    }

    pub fn on_after_command<F, Fut>(&mut self, hook: F)
    where
        F: Fn(Context, String, Result<(), TweezerError>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.after_command_hook = Some(Arc::new(move |ctx, cmd, result| Box::pin(hook(ctx, cmd, result))));
    }

    pub fn on_unrecognized_command<F, Fut>(&mut self, hook: F)
    where
        F: Fn(Context, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TweezerError>> + Send + 'static,
    {
        self.unrecognized_command_hook = Some(Arc::new(move |ctx, cmd| Box::pin(hook(ctx, cmd))));
    }

    pub fn add_command<F, Fut>(&mut self, cmd: Command<F, Fut>)
    where
        F: Fn(Context) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TweezerError>> + Send + 'static,
    {
        let entry = CommandEntry {
            handler: Arc::new(move |ctx| Box::pin((cmd.handler)(ctx))),
            description: cmd.description,
            category: cmd.category,
            platform: cmd.platform.clone(),
            cooldown: Arc::new(CooldownState::new(cmd.cooldown, cmd.user_cooldown)),
        };
        match (cmd.channel, cmd.platform) {
            (Some(ch), _) => {
                self.channel_commands.insert((ch, cmd.name), entry);
            }
            (None, Some(p)) => {
                self.platform_commands.insert((p, cmd.name), entry);
            }
            (None, None) => {
                self.commands.insert(cmd.name, entry);
            }
        }
    }

    /// Register an auto-generated `!help` command.
    ///
    /// **Must be called after all other commands have been registered.** The
    /// help listing is snapshotted at registration time, so any commands added
    /// after this call will not appear in the output.
    pub fn help_command(&mut self) {
        let global: Vec<(String, Option<String>, Option<String>)> = self
            .commands
            .iter()
            .map(|(name, entry)| {
                (
                    name.clone(),
                    entry.description.clone(),
                    entry.category.clone(),
                )
            })
            .collect();
        let per_channel: Vec<(String, String, Option<String>, Option<String>)> = self
            .channel_commands
            .iter()
            .map(|((ch, name), entry)| {
                (
                    ch.clone(),
                    name.clone(),
                    entry.description.clone(),
                    entry.category.clone(),
                )
            })
            .collect();
        let per_platform: Vec<(String, String, Option<String>, Option<String>)> = self
            .platform_commands
            .iter()
            .map(|((p, name), entry)| {
                (
                    p.clone(),
                    name.clone(),
                    entry.description.clone(),
                    entry.category.clone(),
                )
            })
            .collect();
        let prefix = self.command_prefix;

        self.add_command(Command::new("help", move |ctx| {
            let global = global.clone();
            let per_channel = per_channel.clone();
            let per_platform = per_platform.clone();
            async move {
                let mut available: Vec<(String, Option<String>, Option<String>)> = Vec::new();

                for (name, desc, cat) in &global {
                    if name != "help" {
                        available.push((name.clone(), desc.clone(), cat.clone()));
                    }
                }

                for (ch, name, desc, cat) in &per_channel {
                    if ch == ctx.channel() {
                        available.push((name.clone(), desc.clone(), cat.clone()));
                    }
                }

                for (p, name, desc, cat) in &per_platform {
                    if p == ctx.platform() {
                        available.push((name.clone(), desc.clone(), cat.clone()));
                    }
                }

                if available.is_empty() {
                    ctx.reply("no commands available").await?;
                    return Ok(());
                }

                available.sort_by(|a, b| a.0.cmp(&b.0));

                let mut groups: HashMap<
                    Option<String>,
                    Vec<&(String, Option<String>, Option<String>)>,
                > = HashMap::new();
                for item in &available {
                    groups.entry(item.2.clone()).or_default().push(item);
                }

                let mut categories: Vec<Option<String>> = groups.keys().cloned().collect();
                categories.sort_by(|a, b| match (a, b) {
                    (None, _) => std::cmp::Ordering::Less,
                    (_, None) => std::cmp::Ordering::Greater,
                    (Some(a), Some(b)) => a.cmp(b),
                });

                let mut parts: Vec<String> = Vec::new();
                for cat in &categories {
                    let items = groups.get(cat).unwrap();
                    let cmd_strs: Vec<String> = items
                        .iter()
                        .map(|(name, desc, _)| match desc {
                            Some(d) => format!("{}{name}: {d}", prefix),
                            None => format!("{}{name}", prefix),
                        })
                        .collect();
                    match cat {
                        Some(c) => parts.push(format!("{c}: {}", cmd_strs.join(", "))),
                        None => parts.push(cmd_strs.join(", ")),
                    }
                }

                ctx.reply(&parts.join(" | ")).await
            }
        }));
    }

    /// Register an auto-generated `!help` command with a custom formatter.
    ///
    /// **Must be called after all other commands have been registered.**
    pub fn help_command_with<F>(&mut self, formatter: F)
    where
        F: Fn(&[HelpEntry], &str, char) -> String + Send + Sync + 'static,
    {
        let formatter = Arc::new(formatter);
        let global: Vec<HelpEntry> = self
            .commands
            .iter()
            .map(|(name, entry)| HelpEntry {
                name: name.clone(),
                description: entry.description.clone(),
                category: entry.category.clone(),
            })
            .collect();
        let per_channel: Vec<(String, HelpEntry)> = self
            .channel_commands
            .iter()
            .map(|((ch, name), entry)| {
                (
                    ch.clone(),
                    HelpEntry {
                        name: name.clone(),
                        description: entry.description.clone(),
                        category: entry.category.clone(),
                    },
                )
            })
            .collect();
        let per_platform: Vec<(String, HelpEntry)> = self
            .platform_commands
            .iter()
            .map(|((p, name), entry)| {
                (
                    p.clone(),
                    HelpEntry {
                        name: name.clone(),
                        description: entry.description.clone(),
                        category: entry.category.clone(),
                    },
                )
            })
            .collect();
        let prefix = self.command_prefix;

        self.add_command(Command::new("help", move |ctx| {
            let formatter = formatter.clone();
            let global = global.clone();
            let per_channel = per_channel.clone();
            let per_platform = per_platform.clone();
            async move {
                let mut available: Vec<HelpEntry> =
                    global.into_iter().filter(|e| e.name != "help").collect();

                for (ch, entry) in &per_channel {
                    if ch == ctx.channel() {
                        available.push(entry.clone());
                    }
                }

                for (p, entry) in &per_platform {
                    if p == ctx.platform() {
                        available.push(entry.clone());
                    }
                }

                let text = formatter(&available, ctx.channel(), prefix);
                ctx.reply(&text).await
            }
        }));
    }

    pub fn uptime_command(&mut self) {
        let started_at = self.started_at;

        self.add_command(
            Command::new("uptime", move |ctx| async move {
                let elapsed = started_at.elapsed();
                let secs = elapsed.as_secs();
                let days = secs / 86400;
                let hours = (secs % 86400) / 3600;
                let mins = (secs % 3600) / 60;
                let s = secs % 60;

                let text = match (days, hours, mins) {
                    (0, 0, 0) => format!("{}s", s),
                    (0, 0, _) => format!("{}m {}s", mins, s),
                    (0, _, _) => format!("{}h {}m {}s", hours, mins, s),
                    (_, _, _) => format!("{}d {}h {}m {}s", days, hours, mins, s),
                };

                ctx.reply(&text).await
            })
            .description("how long the bot has been running"),
        );
    }

    pub fn about_command(&mut self) {
        let version = env!("CARGO_PKG_VERSION").to_string();

        self.add_command(
            Command::new("about", move |ctx| {
                let version = version.clone();
                async move { ctx.reply(&format!("tweezer v{}", version)).await }
            })
            .description("bot version info"),
        );
    }

    pub fn on_raw_message<F, Fut>(&mut self, handler: F)
    where
        F: Fn(Context) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TweezerError>> + Send + 'static,
    {
        self.raw_message_handlers
            .push(Arc::new(move |ctx| Box::pin(handler(ctx))));
    }

    pub fn on_raw_message_for<F, Fut>(&mut self, channel: &str, handler: F)
    where
        F: Fn(Context) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TweezerError>> + Send + 'static,
    {
        let channel = channel.to_string();
        self.raw_message_handlers.push(Arc::new(move |ctx| {
            if ctx.channel() == channel {
                Box::pin(handler(ctx))
            } else {
                Box::pin(async move { Ok(()) })
            }
        }));
    }

    pub fn on_raw_message_platform<F, Fut>(&mut self, platform: &str, handler: F)
    where
        F: Fn(Context) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TweezerError>> + Send + 'static,
    {
        let platform = platform.to_string();
        self.raw_message_handlers.push(Arc::new(move |ctx| {
            if ctx.platform() == platform {
                Box::pin(handler(ctx))
            } else {
                Box::pin(async move { Ok(()) })
            }
        }));
    }

    pub fn on_trigger<F, Fut>(&mut self, handler: F)
    where
        F: Fn(TriggerContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TweezerError>> + Send + 'static,
    {
        self.on_trigger_matched(TriggerMatcher::Any, handler);
    }

    pub fn on_raid<F, Fut>(&mut self, handler: F)
    where
        F: Fn(TriggerContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TweezerError>> + Send + 'static,
    {
        self.on_trigger_matched(TriggerMatcher::AnyRaid, handler);
    }

    pub fn on_follow<F, Fut>(&mut self, handler: F)
    where
        F: Fn(TriggerContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TweezerError>> + Send + 'static,
    {
        self.on_trigger_matched(TriggerMatcher::AnyFollow, handler);
    }

    pub fn on_subscription<F, Fut>(&mut self, handler: F)
    where
        F: Fn(TriggerContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TweezerError>> + Send + 'static,
    {
        self.on_trigger_matched(TriggerMatcher::AnySubscription, handler);
    }

    pub fn on_donation<F, Fut>(&mut self, handler: F)
    where
        F: Fn(TriggerContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TweezerError>> + Send + 'static,
    {
        self.on_trigger_matched(TriggerMatcher::AnyDonation, handler);
    }

    pub fn on_platform_trigger<F, Fut>(&mut self, kind_id: &str, handler: F)
    where
        F: Fn(TriggerContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TweezerError>> + Send + 'static,
    {
        self.on_trigger_matched(TriggerMatcher::PlatformKind(kind_id.to_string()), handler);
    }

    fn on_trigger_matched<F, Fut>(&mut self, matcher: TriggerMatcher, handler: F)
    where
        F: Fn(TriggerContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TweezerError>> + Send + 'static,
    {
        self.trigger_handlers
            .push((matcher, Arc::new(move |ctx| Box::pin(handler(ctx)))));
    }

    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut warnings = Vec::new();

        if self.adapters.is_empty() {
            warnings.push("no adapters registered; bot will start but won't receive messages".into());
        }

        if self.commands.is_empty()
            && self.channel_commands.is_empty()
            && self.platform_commands.is_empty()
            && self.raw_message_handlers.is_empty()
            && self.trigger_handlers.is_empty()
        {
            warnings.push("no commands, raw message handlers, or trigger handlers registered".into());
        }

        if warnings.is_empty() {
            Ok(())
        } else {
            Err(warnings)
        }
    }

    pub async fn run(mut self) -> Result<(), TweezerError> {
        if let Err(warnings) = self.validate() {
            for w in &warnings {
                eprintln!("[tweezer] warning: {w}");
            }
        }

        let (tx, mut rx) = mpsc::channel::<Event>(256);

        for adapter in &mut self.adapters {
            adapter.connect(tx.clone()).await?;
        }
        drop(tx);

        let commands = Arc::new(self.commands);
        let channel_commands = Arc::new(self.channel_commands);
        let platform_commands = Arc::new(self.platform_commands);
        let raw_message_handlers = Arc::new(self.raw_message_handlers);
        let trigger_handlers = Arc::new(self.trigger_handlers);
        let error_hook: Arc<dyn Fn(HandlerError) + Send + Sync> = self
            .error_hook
            .unwrap_or_else(|| Arc::new(default_error_handler));
        let before_command_hook = self.before_command_hook;
        let after_command_hook = self.after_command_hook;
        let unrecognized_command_hook = self.unrecognized_command_hook;
        let command_prefix = self.command_prefix;
        let mut shutdown_rx = self.shutdown_rx;
        let semaphore = Arc::new(Semaphore::new(self.max_concurrency));
        let state = Arc::new(self.state);

        let mut join_set: JoinSet<()> = JoinSet::new();

        loop {
            tokio::select! {
                event = rx.recv() => {
                    match event {
                        Some(event) => Self::dispatch(
                            event, &commands, &channel_commands, &platform_commands,
                            &raw_message_handlers, &trigger_handlers, &error_hook, command_prefix,
                            &before_command_hook, &after_command_hook, &unrecognized_command_hook,
                            &semaphore, &state, &mut join_set,
                        ),
                        None => break,
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }
            }
        }

        join_set.join_all().await;

        Ok(())
    }

    fn dispatch(
        event: Event,
        commands: &Arc<HashMap<String, CommandEntry>>,
        channel_commands: &Arc<HashMap<(String, String), CommandEntry>>,
        platform_commands: &Arc<HashMap<(String, String), CommandEntry>>,
        raw_message_handlers: &Arc<Vec<Handler>>,
        trigger_handlers: &Arc<Vec<(TriggerMatcher, TriggerHandler)>>,
        error_hook: &Arc<dyn Fn(HandlerError) + Send + Sync>,
        command_prefix: char,
        before_command_hook: &Option<BeforeCommandHook>,
        after_command_hook: &Option<AfterCommandHook>,
        unrecognized_command_hook: &Option<UnrecognizedCommandHook>,
        semaphore: &Arc<Semaphore>,
        state: &Arc<TypeMap>,
        join_set: &mut JoinSet<()>,
    ) {
        match event {
            Event::Message(msg) => {
                let ctx = Context::new(
                    msg.text.clone(),
                    msg.user,
                    msg.platform,
                    msg.channel,
                    msg.reply_tx,
                    msg.emote_fn,
                    state.clone(),
                    msg.max_reply_graphemes,
                );

                for handler in raw_message_handlers.iter() {
                    let handler = handler.clone();
                    let ctx = ctx.clone();
                    let hook = error_hook.clone();
                    let sem = semaphore.clone();
                    join_set.spawn(async move {
                        let _permit = sem.acquire().await.unwrap();
                        let platform = ctx.platform().to_string();
                        let channel = ctx.channel().to_string();
                        if let Err(e) = handler(ctx).await {
                            hook(HandlerError {
                                kind: HandlerErrorKind::RawMessage,
                                platform,
                                channel,
                                error: e,
                            });
                        }
                    });
                }

                if let Some(rest) = msg.text.strip_prefix(command_prefix) {
                    let mut parts = rest.splitn(2, char::is_whitespace);
                    let cmd = parts.next().unwrap_or("").to_string();
                    let args: Vec<String> = match parts.next() {
                        Some(s) if !s.is_empty() => {
                            s.split_whitespace().map(String::from).collect()
                        }
                        _ => Vec::new(),
                    };
                    if cmd.is_empty() {
                        return;
                    }
                    // Lookup priority: channel-specific -> platform-specific -> global
                    let entry = channel_commands
                        .get(&(ctx.channel().to_string(), cmd.clone()))
                        .map(|e| (e.handler.clone(), e.cooldown.clone()))
                        .or_else(|| {
                            platform_commands
                                .get(&(ctx.platform().to_string(), cmd.clone()))
                                .map(|e| (e.handler.clone(), e.cooldown.clone()))
                        })
                        .or_else(|| {
                            commands.get(&cmd).map(|e| (e.handler.clone(), e.cooldown.clone()))
                        });
                    if let Some((handler, cooldown)) = entry {
                        if !cooldown.check_and_mark(&ctx.user.id) {
                            return;
                        }
                        let ctx = ctx.with_args(args);

                        if let Some(before_hook) = before_command_hook {
                            let before_hook = before_hook.clone();
                            let before_ctx = ctx.clone();
                            let before_cmd = cmd.clone();
                            let handler = handler.clone();
                            let hook = error_hook.clone();
                            let after_hook = after_command_hook.clone();
                            let sem = semaphore.clone();
                            let platform = ctx.platform().to_string();
                            let channel = ctx.channel().to_string();
                            join_set.spawn(async move {
                                let _permit = sem.acquire().await.unwrap();
                                if !before_hook(before_ctx, before_cmd).await {
                                    return;
                                }
                                let result = handler(ctx.clone()).await;
                                if let Some(after) = after_hook {
                                    after(ctx.clone(), cmd.clone(), result.clone()).await;
                                }
                                if let Err(e) = result {
                                    hook(HandlerError {
                                        kind: HandlerErrorKind::Command { name: cmd },
                                        platform,
                                        channel,
                                        error: e,
                                    });
                                }
                            });
                        } else {
                            let hook = error_hook.clone();
                            let after_hook = after_command_hook.clone();
                            let sem = semaphore.clone();
                            join_set.spawn(async move {
                                let _permit = sem.acquire().await.unwrap();
                                let platform = ctx.platform().to_string();
                                let channel = ctx.channel().to_string();
                                let result = handler(ctx.clone()).await;
                                if let Some(after) = after_hook {
                                    after(ctx, cmd.clone(), result.clone()).await;
                                }
                                if let Err(e) = result {
                                    hook(HandlerError {
                                        kind: HandlerErrorKind::Command { name: cmd },
                                        platform,
                                        channel,
                                        error: e,
                                    });
                                }
                            });
                        }
                    } else if let Some(unrecognized_hook) = unrecognized_command_hook {
                        let unrecognized_hook = unrecognized_hook.clone();
                        let ctx = ctx.with_args(args);
                        let hook = error_hook.clone();
                        let sem = semaphore.clone();
                        join_set.spawn(async move {
                            let _permit = sem.acquire().await.unwrap();
                            let platform = ctx.platform().to_string();
                            let channel = ctx.channel().to_string();
                            if let Err(e) = unrecognized_hook(ctx, cmd).await {
                                hook(HandlerError {
                                    kind: HandlerErrorKind::UnrecognizedCommand,
                                    platform,
                                    channel,
                                    error: e,
                                });
                            }
                        });
                    }
                }
            }

            Event::Trigger(trigger_event) => {
                let ctx = TriggerContext::new(trigger_event, state.clone());
                for (matcher, handler) in trigger_handlers.iter() {
                    if matcher.matches(&ctx.kind) {
                        let handler = handler.clone();
                        let ctx = ctx.clone();
                        let hook = error_hook.clone();
                        let sem = semaphore.clone();
                        join_set.spawn(async move {
                            let _permit = sem.acquire().await.unwrap();
                            let platform = ctx.platform().to_string();
                            let channel = ctx.channel().to_string();
                            if let Err(e) = handler(ctx).await {
                                hook(HandlerError {
                                    kind: HandlerErrorKind::Trigger,
                                    platform,
                                    channel,
                                    error: e,
                                });
                            }
                        });
                    }
                }
            }
        }
    }
}

impl Default for Bot {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::sync::mpsc;

    use crate::{
        BotTx, Event, IncomingMessage, OutgoingMessage, TweezerError, User,
        trigger::{TriggerEvent, TriggerKind},
    };

    use super::*;

    fn make_message(text: &str, reply_tx: mpsc::Sender<OutgoingMessage>) -> Event {
        make_message_in(text, "general", reply_tx)
    }

    fn make_message_in(
        text: &str,
        channel: &str,
        reply_tx: mpsc::Sender<OutgoingMessage>,
    ) -> Event {
        Event::Message(IncomingMessage {
            platform: "test".into(),
            user: User {
                name: "alice".into(),
                id: "1".into(),
                display_name: None,
            },
            text: text.into(),
            channel: channel.into(),
            reply_tx,
            emote_fn: Arc::new(|name| format!(":{}:", name)),
            max_reply_graphemes: None,
        })
    }

    fn make_trigger(kind: TriggerKind, reply_tx: mpsc::Sender<OutgoingMessage>) -> Event {
        Event::Trigger(TriggerEvent {
            platform: "test".into(),
            channel: "general".into(),
            kind,
            reply_tx,
            emote_fn: Arc::new(|name| format!(":{}:", name)),
            max_reply_graphemes: None,
        })
    }

    // Drives the bot's run loop with a fixed set of events, then drops the
    // sender so the loop terminates naturally. Returns only after all spawned
    // handler tasks have completed (via JoinSet::join_all).
    async fn run_with_events(bot: Bot, events: Vec<Event>) {
        struct DirectAdapter {
            events: Mutex<Vec<Event>>,
        }

        #[async_trait::async_trait]
        impl Adapter for DirectAdapter {
            fn platform_name(&self) -> &str {
                "test"
            }
            fn emote_fn(&self) -> Arc<dyn Fn(&str) -> String + Send + Sync> {
                Arc::new(|name| format!(":{}:", name))
            }
            async fn connect(&mut self, bot: BotTx) -> Result<(), TweezerError> {
                let events = std::mem::take(&mut *self.events.lock().unwrap());
                for event in events {
                    bot.send(event).await.ok();
                }
                Ok(())
            }
        }

        let mut bot = bot;
        bot.add_adapter(DirectAdapter {
            events: Mutex::new(events),
        });
        bot.run().await.unwrap();
    }

    // -----------------------------------------------------------------------
    // Message / command tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn command_handler_fires_on_bang_prefix() {
        let fired = Arc::new(Mutex::new(false));
        let fired_clone = fired.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("ping", move |_ctx| {
            let fired = fired_clone.clone();
            async move {
                *fired.lock().unwrap() = true;
                Ok(())
            }
        }));

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!ping", reply_tx)]).await;
        assert!(*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn command_handler_does_not_fire_for_plain_message() {
        let fired = Arc::new(Mutex::new(false));
        let fired_clone = fired.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("ping", move |_ctx| {
            let fired = fired_clone.clone();
            async move {
                *fired.lock().unwrap() = true;
                Ok(())
            }
        }));

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("hello", reply_tx)]).await;
        assert!(!*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn on_raw_message_fires_for_all_messages() {
        let count = Arc::new(Mutex::new(0u32));
        let count_clone = count.clone();

        let mut bot = Bot::new();
        bot.on_raw_message(move |_ctx| {
            let count = count_clone.clone();
            async move {
                *count.lock().unwrap() += 1;
                Ok(())
            }
        });

        let (tx1, _) = mpsc::channel(1);
        let (tx2, _) = mpsc::channel(1);
        run_with_events(
            bot,
            vec![make_message("hello", tx1), make_message("!ping", tx2)],
        )
        .await;
        assert_eq!(*count.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn reply_sends_to_reply_tx() {
        let mut bot = Bot::new();
        bot.add_command(Command::new("echo", |ctx| async move {
            ctx.reply("pong").await
        }));

        let (reply_tx, mut reply_rx) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!echo", reply_tx)]).await;

        let msg = reply_rx.try_recv().expect("expected a reply");
        assert_eq!(msg.text, "pong");
    }

    #[tokio::test]
    async fn emote_calls_adapter_emote_fn() {
        let mut bot = Bot::new();
        bot.add_command(Command::new("emote", |ctx| async move {
            let e = ctx.emote("pogchamp");
            ctx.reply(&e).await
        }));

        let (reply_tx, mut reply_rx) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!emote", reply_tx)]).await;

        let msg = reply_rx.try_recv().expect("expected a reply");
        assert_eq!(msg.text, ":pogchamp:");
    }

    #[tokio::test]
    async fn command_prefix_is_configurable() {
        let fired = Arc::new(Mutex::new(false));
        let fired_clone = fired.clone();

        let mut bot = Bot::new().command_prefix('/');
        bot.add_command(Command::new("ping", move |_ctx| {
            let fired = fired_clone.clone();
            async move {
                *fired.lock().unwrap() = true;
                Ok(())
            }
        }));

        let (tx1, _) = mpsc::channel(1);
        let (tx2, _) = mpsc::channel(1);
        // /ping should fire; !ping should not
        run_with_events(
            bot,
            vec![make_message("!ping", tx1), make_message("/ping", tx2)],
        )
        .await;
        assert!(*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn error_hook_called_on_command_error() {
        let received = Arc::new(Mutex::new(false));
        let received_clone = received.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("fail", |_ctx| async move {
            Err(TweezerError::Trigger("oops".into()))
        }));
        bot.on_error(move |e| {
            if matches!(e.kind, HandlerErrorKind::Command { .. }) {
                *received_clone.lock().unwrap() = true;
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!fail", reply_tx)]).await;
        assert!(*received.lock().unwrap());
    }

    // -----------------------------------------------------------------------
    // Trigger tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn on_raid_handler_fires_for_raid() {
        let fired = Arc::new(Mutex::new(false));
        let fired_clone = fired.clone();

        let mut bot = Bot::new();
        bot.on_raid(move |_ctx| {
            let fired = fired_clone.clone();
            async move {
                *fired.lock().unwrap() = true;
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        let kind = TriggerKind::Raid {
            from_channel: "friend".into(),
            viewer_count: 10,
        };
        run_with_events(bot, vec![make_trigger(kind, reply_tx)]).await;
        assert!(*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn on_raid_handler_does_not_fire_for_follow() {
        let fired = Arc::new(Mutex::new(false));
        let fired_clone = fired.clone();

        let mut bot = Bot::new();
        bot.on_raid(move |_ctx| {
            let fired = fired_clone.clone();
            async move {
                *fired.lock().unwrap() = true;
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        let kind = TriggerKind::Follow {
            user: User {
                name: "alice".into(),
                id: "1".into(),
                display_name: None,
            },
        };
        run_with_events(bot, vec![make_trigger(kind, reply_tx)]).await;
        assert!(!*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn on_trigger_catch_all_fires_for_multiple_kinds() {
        let count = Arc::new(Mutex::new(0u32));
        let count_clone = count.clone();

        let mut bot = Bot::new();
        bot.on_trigger(move |_ctx| {
            let count = count_clone.clone();
            async move {
                *count.lock().unwrap() += 1;
                Ok(())
            }
        });

        let (tx1, _) = mpsc::channel(1);
        let (tx2, _) = mpsc::channel(1);
        run_with_events(
            bot,
            vec![
                make_trigger(
                    TriggerKind::Raid {
                        from_channel: "friend".into(),
                        viewer_count: 5,
                    },
                    tx1,
                ),
                make_trigger(
                    TriggerKind::Follow {
                        user: User {
                            name: "bob".into(),
                            id: "2".into(),
                            display_name: None,
                        },
                    },
                    tx2,
                ),
            ],
        )
        .await;
        assert_eq!(*count.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn trigger_reply_sends_to_reply_tx() {
        let mut bot = Bot::new();
        bot.on_raid(|ctx| async move { ctx.reply("welcome raiders!").await });

        let (reply_tx, mut reply_rx) = mpsc::channel(1);
        let kind = TriggerKind::Raid {
            from_channel: "friend".into(),
            viewer_count: 10,
        };
        run_with_events(bot, vec![make_trigger(kind, reply_tx)]).await;

        let msg = reply_rx.try_recv().expect("expected a reply");
        assert_eq!(msg.text, "welcome raiders!");
    }

    #[tokio::test]
    async fn on_platform_trigger_matches_by_kind_id() {
        use crate::trigger::PlatformTrigger;

        #[derive(Debug, Clone)]
        struct TestTrigger {
            id: String,
        }
        impl PlatformTrigger for TestTrigger {
            fn kind_id(&self) -> &str {
                &self.id
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
            fn clone_box(&self) -> Box<dyn PlatformTrigger> {
                Box::new(self.clone())
            }
        }

        let fired = Arc::new(Mutex::new(false));
        let fired_clone = fired.clone();

        let mut bot = Bot::new();
        bot.on_platform_trigger("test.custom", move |_ctx| {
            let fired = fired_clone.clone();
            async move {
                *fired.lock().unwrap() = true;
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        let kind = TriggerKind::Platform(Box::new(TestTrigger {
            id: "test.custom".into(),
        }));
        run_with_events(bot, vec![make_trigger(kind, reply_tx)]).await;
        assert!(*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn on_platform_trigger_does_not_fire_for_wrong_kind_id() {
        use crate::trigger::PlatformTrigger;

        #[derive(Debug, Clone)]
        struct TestTrigger {
            id: String,
        }
        impl PlatformTrigger for TestTrigger {
            fn kind_id(&self) -> &str {
                &self.id
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
            fn clone_box(&self) -> Box<dyn PlatformTrigger> {
                Box::new(self.clone())
            }
        }

        let fired = Arc::new(Mutex::new(false));
        let fired_clone = fired.clone();

        let mut bot = Bot::new();
        bot.on_platform_trigger("test.other", move |_ctx| {
            let fired = fired_clone.clone();
            async move {
                *fired.lock().unwrap() = true;
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        let kind = TriggerKind::Platform(Box::new(TestTrigger {
            id: "test.custom".into(),
        }));
        run_with_events(bot, vec![make_trigger(kind, reply_tx)]).await;
        assert!(!*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn on_subscription_fires_for_sub() {
        let fired = Arc::new(Mutex::new(false));
        let fired_clone = fired.clone();

        let mut bot = Bot::new();
        bot.on_subscription(move |_ctx| {
            let fired = fired_clone.clone();
            async move {
                *fired.lock().unwrap() = true;
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        let kind = TriggerKind::Subscription {
            user: User {
                name: "carol".into(),
                id: "3".into(),
                display_name: None,
            },
            tier: "1".into(),
            months: 1,
            message: None,
        };
        run_with_events(bot, vec![make_trigger(kind, reply_tx)]).await;
        assert!(*fired.lock().unwrap());
    }

    // -----------------------------------------------------------------------
    // Channel-scoped command tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn command_for_fires_on_matching_channel() {
        let fired = Arc::new(Mutex::new(false));
        let fired_clone = fired.clone();

        let mut bot = Bot::new();
        bot.add_command(
            Command::new("ping", move |_ctx| {
                let fired = fired_clone.clone();
                async move {
                    *fired.lock().unwrap() = true;
                    Ok(())
                }
            })
            .channel("streamer-a"),
        );

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message_in("!ping", "streamer-a", reply_tx)]).await;
        assert!(*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn command_for_does_not_fire_on_wrong_channel() {
        let fired = Arc::new(Mutex::new(false));
        let fired_clone = fired.clone();

        let mut bot = Bot::new();
        bot.add_command(
            Command::new("ping", move |_ctx| {
                let fired = fired_clone.clone();
                async move {
                    *fired.lock().unwrap() = true;
                    Ok(())
                }
            })
            .channel("streamer-a"),
        );

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message_in("!ping", "streamer-b", reply_tx)]).await;
        assert!(!*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn command_for_takes_precedence_over_global() {
        let channel_fired = Arc::new(Mutex::new(false));
        let global_fired = Arc::new(Mutex::new(false));
        let cf = channel_fired.clone();
        let gf = global_fired.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("ping", move |_ctx| {
            let gf = gf.clone();
            async move {
                *gf.lock().unwrap() = true;
                Ok(())
            }
        }));
        bot.add_command(
            Command::new("ping", move |_ctx| {
                let cf = cf.clone();
                async move {
                    *cf.lock().unwrap() = true;
                    Ok(())
                }
            })
            .channel("streamer-a"),
        );

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message_in("!ping", "streamer-a", reply_tx)]).await;
        assert!(*channel_fired.lock().unwrap());
        assert!(!*global_fired.lock().unwrap());
    }

    #[tokio::test]
    async fn global_command_fires_when_no_channel_command_matches() {
        let global_fired = Arc::new(Mutex::new(false));
        let gf = global_fired.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("ping", move |_ctx| {
            let gf = gf.clone();
            async move {
                *gf.lock().unwrap() = true;
                Ok(())
            }
        }));
        bot.add_command(
            Command::new("ping", move |_ctx| async move { Ok(()) }).channel("streamer-a"),
        );

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message_in("!ping", "streamer-b", reply_tx)]).await;
        assert!(*global_fired.lock().unwrap());
    }

    #[tokio::test]
    async fn multiple_commands_fire_independently() {
        let ping_fired = Arc::new(Mutex::new(false));
        let help_fired = Arc::new(Mutex::new(false));
        let pf = ping_fired.clone();
        let hf = help_fired.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("ping", move |_ctx| {
            let pf = pf.clone();
            async move {
                *pf.lock().unwrap() = true;
                Ok(())
            }
        }));
        bot.add_command(Command::new("help", move |_ctx| {
            let hf = hf.clone();
            async move {
                *hf.lock().unwrap() = true;
                Ok(())
            }
        }));

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!ping", reply_tx)]).await;
        assert!(*ping_fired.lock().unwrap());
        assert!(!*help_fired.lock().unwrap());
    }

    #[tokio::test]
    async fn command_with_args_strips_name_correctly() {
        let got = Arc::new(Mutex::new(String::new()));
        let g = got.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("echo", move |ctx| {
            let g = g.clone();
            async move {
                *g.lock().unwrap() = ctx.message.clone();
                Ok(())
            }
        }));

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!echo hello world", reply_tx)]).await;
        assert_eq!(*got.lock().unwrap(), "!echo hello world");
    }

    #[tokio::test]
    async fn unknown_command_ignored() {
        let fired = Arc::new(Mutex::new(false));
        let f = fired.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("ping", move |_ctx| {
            let f = f.clone();
            async move {
                *f.lock().unwrap() = true;
                Ok(())
            }
        }));

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!unknown", reply_tx)]).await;
        assert!(!*fired.lock().unwrap());
    }

    // -----------------------------------------------------------------------
    // on_before_command / on_after_command / on_unrecognized_command tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn before_command_fires_before_handler() {
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let o1 = order.clone();
        let o2 = order.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("ping", move |_ctx| {
            let o = o2.clone();
            async move {
                o.lock().unwrap().push("handler".into());
                Ok(())
            }
        }));
        bot.on_before_command(move |_ctx, cmd| {
            let o = o1.clone();
            async move {
                o.lock().unwrap().push(format!("before:{cmd}"));
                true
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!ping", reply_tx)]).await;
        let got = order.lock().unwrap().clone();
        assert_eq!(got, vec!["before:ping", "handler"]);
    }

    #[tokio::test]
    async fn before_command_returning_false_skips_handler() {
        let handler_fired = Arc::new(Mutex::new(false));
        let hf = handler_fired.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("ping", move |_ctx| {
            let hf = hf.clone();
            async move {
                *hf.lock().unwrap() = true;
                Ok(())
            }
        }));
        bot.on_before_command(|_ctx, _cmd| async { false });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!ping", reply_tx)]).await;
        assert!(!*handler_fired.lock().unwrap());
    }

    #[tokio::test]
    async fn after_command_fires_after_handler_success() {
        let got_result = Arc::new(Mutex::new(None));
        let gr = got_result.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("ping", |_ctx| async { Ok(()) }));
        bot.on_after_command(move |_ctx, cmd, result| {
            let gr = gr.clone();
            async move {
                *gr.lock().unwrap() = Some((cmd, result.is_ok()));
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!ping", reply_tx)]).await;
        let got = got_result.lock().unwrap().clone();
        assert_eq!(got, Some(("ping".into(), true)));
    }

    #[tokio::test]
    async fn after_command_fires_after_handler_error() {
        let got_result = Arc::new(Mutex::new(None));
        let gr = got_result.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("fail", |_ctx| async {
            Err(TweezerError::Trigger("oops".into()))
        }));
        bot.on_after_command(move |_ctx, cmd, result| {
            let gr = gr.clone();
            async move {
                *gr.lock().unwrap() = Some((cmd, result.is_err()));
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!fail", reply_tx)]).await;
        let got = got_result.lock().unwrap().clone();
        assert_eq!(got, Some(("fail".into(), true)));
    }

    #[tokio::test]
    async fn unrecognized_command_fires_for_unknown_name() {
        let got_cmd = Arc::new(Mutex::new(String::new()));
        let gc = got_cmd.clone();

        let mut bot = Bot::new();
        bot.on_unrecognized_command(move |_ctx, cmd| {
            let gc = gc.clone();
            async move {
                *gc.lock().unwrap() = cmd;
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!nope", reply_tx)]).await;
        assert_eq!(*got_cmd.lock().unwrap(), "nope");
    }

    #[tokio::test]
    async fn unrecognized_command_does_not_fire_for_known_command() {
        let fired = Arc::new(Mutex::new(false));
        let f = fired.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("ping", |_ctx| async { Ok(()) }));
        bot.on_unrecognized_command(move |_ctx, _cmd| {
            let f = f.clone();
            async move {
                *f.lock().unwrap() = true;
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!ping", reply_tx)]).await;
        assert!(!*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn unrecognized_command_does_not_fire_for_plain_message() {
        let fired = Arc::new(Mutex::new(false));
        let f = fired.clone();

        let mut bot = Bot::new();
        bot.on_unrecognized_command(move |_ctx, _cmd| {
            let f = f.clone();
            async move {
                *f.lock().unwrap() = true;
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("hello", reply_tx)]).await;
        assert!(!*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn unrecognized_command_does_not_fire_for_bare_prefix() {
        let fired = Arc::new(Mutex::new(false));
        let f = fired.clone();

        let mut bot = Bot::new();
        bot.on_unrecognized_command(move |_ctx, _cmd| {
            let f = f.clone();
            async move {
                *f.lock().unwrap() = true;
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!", reply_tx)]).await;
        assert!(!*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn unrecognized_command_receives_args() {
        let got_args = Arc::new(Mutex::new(Vec::<String>::new()));
        let ga = got_args.clone();

        let mut bot = Bot::new();
        bot.on_unrecognized_command(move |ctx, _cmd| {
            let ga = ga.clone();
            async move {
                *ga.lock().unwrap() = ctx.args().to_vec();
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!nope a b c", reply_tx)]).await;
        assert_eq!(*got_args.lock().unwrap(), vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn on_raw_message_and_command_both_fire() {
        let msg_count = Arc::new(Mutex::new(0u32));
        let cmd_fired = Arc::new(Mutex::new(false));
        let mc = msg_count.clone();
        let cf = cmd_fired.clone();

        let mut bot = Bot::new();
        bot.on_raw_message(move |_ctx| {
            let mc = mc.clone();
            async move {
                *mc.lock().unwrap() += 1;
                Ok(())
            }
        });
        bot.add_command(Command::new("ping", move |_ctx| {
            let cf = cf.clone();
            async move {
                *cf.lock().unwrap() = true;
                Ok(())
            }
        }));

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!ping", reply_tx)]).await;
        assert_eq!(*msg_count.lock().unwrap(), 1);
        assert!(*cmd_fired.lock().unwrap());
    }

    #[tokio::test]
    async fn multiple_raw_message_handlers_all_fire() {
        let count = Arc::new(Mutex::new(0u32));
        let c1 = count.clone();
        let c2 = count.clone();

        let mut bot = Bot::new();
        bot.on_raw_message(move |_ctx| {
            let c = c1.clone();
            async move {
                *c.lock().unwrap() += 1;
                Ok(())
            }
        });
        bot.on_raw_message(move |_ctx| {
            let c = c2.clone();
            async move {
                *c.lock().unwrap() += 1;
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("hi", reply_tx)]).await;
        assert_eq!(*count.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn multiple_events_processed() {
        let count = Arc::new(Mutex::new(0u32));
        let c = count.clone();

        let mut bot = Bot::new();
        bot.on_raw_message(move |_ctx| {
            let c = c.clone();
            async move {
                *c.lock().unwrap() += 1;
                Ok(())
            }
        });

        let (tx1, _) = mpsc::channel(1);
        let (tx2, _) = mpsc::channel(1);
        let (tx3, _) = mpsc::channel(1);
        run_with_events(
            bot,
            vec![
                make_message("a", tx1),
                make_message("b", tx2),
                make_message("c", tx3),
            ],
        )
        .await;
        assert_eq!(*count.lock().unwrap(), 3);
    }

    #[tokio::test]
    async fn command_for_multiple_channels_independent() {
        let a_fired = Arc::new(Mutex::new(false));
        let b_fired = Arc::new(Mutex::new(false));
        let af = a_fired.clone();
        let bf = b_fired.clone();

        let mut bot = Bot::new();
        bot.add_command(
            Command::new("greet", move |_ctx| {
                let af = af.clone();
                async move {
                    *af.lock().unwrap() = true;
                    Ok(())
                }
            })
            .channel("ch-a"),
        );
        bot.add_command(
            Command::new("greet", move |_ctx| {
                let bf = bf.clone();
                async move {
                    *bf.lock().unwrap() = true;
                    Ok(())
                }
            })
            .channel("ch-b"),
        );

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message_in("!greet", "ch-a", reply_tx)]).await;
        assert!(*a_fired.lock().unwrap());
        assert!(!*b_fired.lock().unwrap());
    }

    #[tokio::test]
    async fn on_donation_fires_for_donation() {
        let fired = Arc::new(Mutex::new(false));
        let f = fired.clone();

        let mut bot = Bot::new();
        bot.on_donation(move |_ctx| {
            let f = f.clone();
            async move {
                *f.lock().unwrap() = true;
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        let kind = TriggerKind::Donation {
            user: User {
                name: "alice".into(),
                id: "1".into(),
                display_name: None,
            },
            amount_cents: 500,
            currency: "USD".into(),
            message: None,
        };
        run_with_events(bot, vec![make_trigger(kind, reply_tx)]).await;
        assert!(*fired.lock().unwrap());
    }

    // -----------------------------------------------------------------------
    // ctx.args() tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn args_empty_for_no_args_command() {
        let got = Arc::new(Mutex::new(Vec::<String>::new()));
        let g = got.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("ping", move |ctx| {
            let g = g.clone();
            async move {
                *g.lock().unwrap() = ctx.args().to_vec();
                Ok(())
            }
        }));

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!ping", reply_tx)]).await;
        assert!(got.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn args_parsed_from_command() {
        let got = Arc::new(Mutex::new(Vec::<String>::new()));
        let g = got.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("echo", move |ctx| {
            let g = g.clone();
            async move {
                *g.lock().unwrap() = ctx.args().to_vec();
                Ok(())
            }
        }));

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!echo hello world", reply_tx)]).await;
        assert_eq!(*got.lock().unwrap(), vec!["hello", "world"]);
    }

    #[tokio::test]
    async fn args_empty_for_on_raw_message_handlers() {
        let got = Arc::new(Mutex::new(false));
        let g = got.clone();

        let mut bot = Bot::new();
        bot.on_raw_message(move |ctx| {
            let g = g.clone();
            async move {
                *g.lock().unwrap() = ctx.args().is_empty();
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("!ping hello", reply_tx)]).await;
        assert!(*got.lock().unwrap());
    }

    // -----------------------------------------------------------------------
    // on_raw_message_for / on_raw_message_platform tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn on_raw_message_for_fires_on_matching_channel() {
        let fired = Arc::new(Mutex::new(false));
        let f = fired.clone();

        let mut bot = Bot::new();
        bot.on_raw_message_for("ch-a", move |_ctx| {
            let f = f.clone();
            async move {
                *f.lock().unwrap() = true;
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message_in("hi", "ch-a", reply_tx)]).await;
        assert!(*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn on_raw_message_for_does_not_fire_on_wrong_channel() {
        let fired = Arc::new(Mutex::new(false));
        let f = fired.clone();

        let mut bot = Bot::new();
        bot.on_raw_message_for("ch-a", move |_ctx| {
            let f = f.clone();
            async move {
                *f.lock().unwrap() = true;
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message_in("hi", "ch-b", reply_tx)]).await;
        assert!(!*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn on_raw_message_platform_fires_on_matching_platform() {
        let fired = Arc::new(Mutex::new(false));
        let f = fired.clone();

        let mut bot = Bot::new();
        bot.on_raw_message_platform("twitch", move |_ctx| {
            let f = f.clone();
            async move {
                *f.lock().unwrap() = true;
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        let event = Event::Message(IncomingMessage {
            platform: "twitch".into(),
            user: User {
                name: "alice".into(),
                id: "1".into(),
                display_name: None,
            },
            text: "hi".into(),
            channel: "general".into(),
            reply_tx,
            emote_fn: Arc::new(|name| format!(":{}:", name)),
            max_reply_graphemes: None,
        });
        run_with_events(bot, vec![event]).await;
        assert!(*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn on_raw_message_platform_does_not_fire_on_wrong_platform() {
        let fired = Arc::new(Mutex::new(false));
        let f = fired.clone();

        let mut bot = Bot::new();
        bot.on_raw_message_platform("twitch", move |_ctx| {
            let f = f.clone();
            async move {
                *f.lock().unwrap() = true;
                Ok(())
            }
        });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message_in("hi", "general", reply_tx)]).await;
        assert!(!*fired.lock().unwrap());
    }

    // -----------------------------------------------------------------------
    // Graceful shutdown test
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn shutdown_handle_stops_run_loop() {
        let mut bot = Bot::new();
        let handle = bot.shutdown_handle();

        bot.on_raw_message(move |_ctx| async move { Ok(()) });

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            handle.shutdown();
        });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message("hi", reply_tx)]).await;
    }

    // -----------------------------------------------------------------------
    // HandlerError carries platform/channel
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn error_hook_receives_platform_and_channel() {
        let got_platform = Arc::new(Mutex::new(String::new()));
        let got_channel = Arc::new(Mutex::new(String::new()));
        let gp = got_platform.clone();
        let gc = got_channel.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("fail", |_ctx| async move {
            Err(TweezerError::Trigger("oops".into()))
        }));
        bot.on_error(move |e| {
            *gp.lock().unwrap() = e.platform.clone();
            *gc.lock().unwrap() = e.channel.clone();
        });

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(bot, vec![make_message_in("!fail", "my-channel", reply_tx)]).await;
        assert_eq!(*got_platform.lock().unwrap(), "test");
        assert_eq!(*got_channel.lock().unwrap(), "my-channel");
    }

    // -----------------------------------------------------------------------
    // help_command tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn help_command_lists_global_commands() {
        let mut bot = Bot::new();
        bot.add_command(
            Command::new("ping", |_ctx| async { Ok(()) }).description("responds with pong"),
        );
        bot.add_command(Command::new("echo", |_ctx| async { Ok(()) }).description("repeats text"));
        bot.help_command();

        let (reply_tx, mut reply_rx) = mpsc::channel(4);
        run_with_events(bot, vec![make_message("!help", reply_tx)]).await;

        let msg = reply_rx.try_recv().expect("expected a reply");
        assert!(msg.text.contains("!echo: repeats text"));
        assert!(msg.text.contains("!ping: responds with pong"));
        assert!(!msg.text.contains("!help"));
    }

    #[tokio::test]
    async fn help_command_includes_channel_commands() {
        let mut bot = Bot::new();
        bot.add_command(
            Command::new("ping", |_ctx| async { Ok(()) }).description("responds with pong"),
        );
        bot.add_command(
            Command::new("timer", |_ctx| async { Ok(()) })
                .description("what is the timer?")
                .channel("ch-a"),
        );
        bot.help_command();

        let (reply_tx, mut reply_rx) = mpsc::channel(4);
        run_with_events(bot, vec![make_message_in("!help", "ch-a", reply_tx)]).await;

        let msg = reply_rx.try_recv().expect("expected a reply");
        assert!(msg.text.contains("!ping: responds with pong"));
        assert!(msg.text.contains("!timer: what is the timer?"));
    }

    #[tokio::test]
    async fn help_command_excludes_other_channel_commands() {
        let mut bot = Bot::new();
        bot.add_command(
            Command::new("timer", |_ctx| async { Ok(()) })
                .description("timer for a")
                .channel("ch-a"),
        );
        bot.add_command(
            Command::new("timer", |_ctx| async { Ok(()) })
                .description("timer for b")
                .channel("ch-b"),
        );
        bot.help_command();

        let (reply_tx, mut reply_rx) = mpsc::channel(4);
        run_with_events(bot, vec![make_message_in("!help", "ch-a", reply_tx)]).await;

        let msg = reply_rx.try_recv().expect("expected a reply");
        assert!(msg.text.contains("!timer: timer for a"));
        assert!(!msg.text.contains("timer for b"));
    }

    #[tokio::test]
    async fn help_command_uses_configured_prefix() {
        let mut bot = Bot::new().command_prefix('/');
        bot.add_command(Command::new("ping", |_ctx| async { Ok(()) }).description("pong"));
        bot.help_command();

        let (reply_tx, mut reply_rx) = mpsc::channel(4);
        run_with_events(bot, vec![make_message_in("/help", "general", reply_tx)]).await;

        let msg = reply_rx.try_recv().expect("expected a reply");
        assert!(msg.text.contains("/ping: pong"));
        assert!(!msg.text.contains("!ping"));
    }

    #[tokio::test]
    async fn help_command_with_custom_formatter() {
        let mut bot = Bot::new();
        bot.add_command(
            Command::new("ping", |_ctx| async { Ok(()) })
                .description("pong")
                .category("general"),
        );
        bot.add_command(
            Command::new("timer", |_ctx| async { Ok(()) })
                .description("what time?")
                .channel("ch-a"),
        );
        bot.help_command_with(|entries, channel, prefix| {
            let names: Vec<String> = entries
                .iter()
                .map(|e| format!("{}{}", prefix, e.name))
                .collect();
            format!(
                "{} commands on {}: {}",
                entries.len(),
                channel,
                names.join(", ")
            )
        });

        let (reply_tx, mut reply_rx) = mpsc::channel(4);
        run_with_events(bot, vec![make_message_in("!help", "ch-a", reply_tx)]).await;

        let msg = reply_rx.try_recv().expect("expected a reply");
        assert!(msg.text.starts_with("2 commands on ch-a:"));
        assert!(msg.text.contains("!ping"));
        assert!(msg.text.contains("!timer"));
    }

    // -----------------------------------------------------------------------
    // uptime / about command tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn uptime_command_replies_with_duration() {
        let mut bot = Bot::new();
        bot.uptime_command();

        let (reply_tx, mut reply_rx) = mpsc::channel(4);
        run_with_events(bot, vec![make_message("!uptime", reply_tx)]).await;

        let msg = reply_rx.try_recv().expect("expected a reply");
        assert!(msg.text.contains('s'));
    }

    #[tokio::test]
    async fn about_command_replies_with_version() {
        let mut bot = Bot::new();
        bot.about_command();

        let (reply_tx, mut reply_rx) = mpsc::channel(4);
        run_with_events(bot, vec![make_message("!about", reply_tx)]).await;

        let msg = reply_rx.try_recv().expect("expected a reply");
        assert!(msg.text.starts_with("tweezer v"));
    }

    fn make_message_on(
        text: &str,
        platform: &str,
        channel: &str,
        reply_tx: mpsc::Sender<OutgoingMessage>,
    ) -> Event {
        Event::Message(IncomingMessage {
            platform: platform.into(),
            user: User {
                name: "alice".into(),
                id: "1".into(),
                display_name: None,
            },
            text: text.into(),
            channel: channel.into(),
            reply_tx,
            emote_fn: Arc::new(|name| format!(":{}:", name)),
            max_reply_graphemes: None,
        })
    }

    #[tokio::test]
    async fn command_for_platform_fires_on_matching_platform() {
        let fired = Arc::new(Mutex::new(false));
        let fired_clone = fired.clone();

        let mut bot = Bot::new();
        bot.add_command(
            Command::new("ping", move |_ctx| {
                let fired = fired_clone.clone();
                async move {
                    *fired.lock().unwrap() = true;
                    Ok(())
                }
            })
            .platform("twitch"),
        );

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(
            bot,
            vec![make_message_on("!ping", "twitch", "channel", reply_tx)],
        )
        .await;
        assert!(*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn command_for_platform_does_not_fire_on_wrong_platform() {
        let fired = Arc::new(Mutex::new(false));
        let fired_clone = fired.clone();

        let mut bot = Bot::new();
        bot.add_command(
            Command::new("ping", move |_ctx| {
                let fired = fired_clone.clone();
                async move {
                    *fired.lock().unwrap() = true;
                    Ok(())
                }
            })
            .platform("twitch"),
        );

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(
            bot,
            vec![make_message_on("!ping", "streamplace", "channel", reply_tx)],
        )
        .await;
        assert!(!*fired.lock().unwrap());
    }

    #[tokio::test]
    async fn platform_command_takes_precedence_over_global() {
        let platform_fired = Arc::new(Mutex::new(false));
        let global_fired = Arc::new(Mutex::new(false));
        let pf = platform_fired.clone();
        let gf = global_fired.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("ping", move |_ctx| {
            let gf = gf.clone();
            async move {
                *gf.lock().unwrap() = true;
                Ok(())
            }
        }));
        bot.add_command(
            Command::new("ping", move |_ctx| {
                let pf = pf.clone();
                async move {
                    *pf.lock().unwrap() = true;
                    Ok(())
                }
            })
            .platform("twitch"),
        );

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(
            bot,
            vec![make_message_on("!ping", "twitch", "channel", reply_tx)],
        )
        .await;
        assert!(*platform_fired.lock().unwrap());
        assert!(!*global_fired.lock().unwrap());
    }

    #[tokio::test]
    async fn global_command_fires_when_no_platform_command_matches() {
        let global_fired = Arc::new(Mutex::new(false));
        let gf = global_fired.clone();

        let mut bot = Bot::new();
        bot.add_command(Command::new("ping", move |_ctx| {
            let gf = gf.clone();
            async move {
                *gf.lock().unwrap() = true;
                Ok(())
            }
        }));
        bot.add_command(Command::new("ping", move |_ctx| async move { Ok(()) }).platform("twitch"));

        let (reply_tx, _) = mpsc::channel(1);
        run_with_events(
            bot,
            vec![make_message_on("!ping", "streamplace", "channel", reply_tx)],
        )
        .await;
        assert!(*global_fired.lock().unwrap());
    }

    #[tokio::test]
    async fn channel_command_takes_precedence_over_platform_command() {
        let channel_fired = Arc::new(Mutex::new(false));
        let platform_fired = Arc::new(Mutex::new(false));
        let cf = channel_fired.clone();
        let pf = platform_fired.clone();

        let mut bot = Bot::new();
        bot.add_command(
            Command::new("ping", move |_ctx| {
                let pf = pf.clone();
                async move {
                    *pf.lock().unwrap() = true;
                    Ok(())
                }
            })
            .platform("twitch"),
        );
        bot.add_command(
            Command::new("ping", move |_ctx| {
                let cf = cf.clone();
                async move {
                    *cf.lock().unwrap() = true;
                    Ok(())
                }
            })
            .channel("special-channel"),
        );

        let (reply_tx, _) = mpsc::channel(1);
        // Message arrives from twitch but in the special channel: channel wins
        run_with_events(
            bot,
            vec![make_message_on(
                "!ping",
                "twitch",
                "special-channel",
                reply_tx,
            )],
        )
        .await;
        assert!(*channel_fired.lock().unwrap());
        assert!(!*platform_fired.lock().unwrap());
    }
}
