mod commands;
mod db;
mod variables;
mod web;

use std::collections::HashSet;

use tracing::info;
use tweezer::{Bot, Command, Context};
use tweezer::command;
use tweezer_streamplace::StreamplaceAdapter;

use db::Database;
use variables::{DynamicCommands, Expander, QuotesDb};

/// User IDs allowed to add/remove dynamic commands.
pub struct Moderators(pub HashSet<String>);

impl Moderators {
    pub fn contains(&self, id: &str) -> bool {
        self.0.contains(id)
    }
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tweezer=info".parse().unwrap()),
        )
        .init();

    let jetstream_url = std::env::var("JETSTREAM_URL")
        .unwrap_or_else(|_| "wss://nyc.firehose.stream/subscribe".into());
    let identifier = std::env::var("BOT_IDENTIFIER").expect("BOT_IDENTIFIER is required");
    let password = std::env::var("BOT_PASSWORD").expect("BOT_PASSWORD is required");

    let streamers =
        std::env::var("STREAMER_DIDS").expect("STREAMER_DIDS is required (comma-separated)");
    let streamer_dids: Vec<String> = streamers.split(',').map(|s| s.trim().to_string()).collect();

    let mut adapter = StreamplaceAdapter::new(jetstream_url, identifier, password);
    adapter.add_streamers(streamer_dids.clone());

    let db_path = std::env::var("DATABASE_PATH").unwrap_or_else(|_| "bot.db".into());
    let database = Database::open(&db_path).expect("failed to open database");

    let mut bot = Bot::new();

    let expander = std::sync::Arc::new(Expander::with_db(database.clone()));
    bot.register(
        DynamicCommands::new(expander.clone())
            .with_db(database.clone()),
    );
    bot.register(Moderators(streamer_dids.into_iter().collect()));

    // Register shared state for commands that need it
    bot.register(commands::pokemon::PokemonCache::new());
    bot.register(commands::pokemon::TypeChartCache::new());
    bot.register(QuotesDb::new().with_db(database.clone()));
    bot.register(commands::polls::PollManager::new());

    let reminder_mgr = commands::reminders::ReminderManager::new(database.clone());
    bot.register(reminder_mgr.clone());

    // ---------------------------------------------------------------------------
    // Static commands (declarative via #[command])
    // ---------------------------------------------------------------------------

    #[command(category = "general")]
    /// responds with pong
    async fn ping(ctx: Context) -> Result<(), tweezer::TweezerError> {
        ctx.reply("pong").await
    }

    #[command(category = "general")]
    /// let everyone know you're going quiet
    async fn lurk(ctx: Context) -> Result<(), tweezer::TweezerError> {
        let name = ctx.user().display().to_string();
        info!("{name} is lurking");
        ctx.reply(&format!("@{name} is now lurking")).await
    }

    #[command(category = "general")]
    /// wat
    async fn wat(ctx: Context) -> Result<(), tweezer::TweezerError> {
        let name = ctx.user().display().to_string();
        ctx.reply(&format!("@{name} z.ai https://github.com")).await
    }

    #[command(
        name = "timer",
        description = "what is the timer?",
        category = "streamplace",
        channel = "did:plc:2zmxikig2sj7gqaezl5gntae"
    )]
    async fn timer_cmd(ctx: Context) -> Result<(), tweezer::TweezerError> {
        ctx.reply(
            "The timer counts down to Unix time 173173173173.173. \
             173 on an upside-down calculator spells Eli.",
        )
        .await
    }

    bot.add_command(ping());
    bot.add_command(lurk());
    bot.add_command(wat());
    bot.add_command(timer_cmd());

    commands::games::add_commands(&mut bot);
    commands::pokemon::add_command(&mut bot);
    commands::ffxiv::add_command(&mut bot);
    commands::genshin::add_command(&mut bot);
    commands::metar::add_command(&mut bot);
    commands::quotes::add_commands(&mut bot);
    commands::poll_commands::add_commands(&mut bot);
    commands::translate::add_command(&mut bot);
    commands::reminders::add_commands(&mut bot);

    // ---------------------------------------------------------------------------
    // Dynamic command management
    // ---------------------------------------------------------------------------

    bot.add_command(
        Command::new("addcmd", |ctx: Context| async move {
            let dc = ctx
                .state::<DynamicCommands>()
                .expect("DynamicCommands not registered");
            let name = match ctx.args().first() {
                Some(n) => n.trim_start_matches('!').to_string(),
                None => return ctx.reply("usage: !addcmd !name template text").await,
            };
            if ctx.args().len() < 2 {
                return ctx.reply("usage: !addcmd !name template text").await;
            }
            let template = ctx.args()[1..].join(" ");
            dc.add(&name, &template);
            ctx.reply(&format!("command !{name} added")).await
        })
        .description("add a dynamic command: !addcmd !name template")
        .category("moderation")
        .check(|ctx| async move {
            ctx.state::<Moderators>()
                .map(|m| m.contains(ctx.user().name.as_str()))
                .unwrap_or(false)
        })
        .on_check_fail(|ctx| async move {
            ctx.reply("only the streamer can add commands").await
        }),
    );

    bot.add_command(
        Command::new("delcmd", |ctx: Context| async move {
            let dc = ctx
                .state::<DynamicCommands>()
                .expect("DynamicCommands not registered");
            let name = match ctx.args().first() {
                Some(n) => n.trim_start_matches('!').to_string(),
                None => return ctx.reply("usage: !delcmd !name").await,
            };
            if dc.remove(&name) {
                ctx.reply(&format!("command !{name} removed")).await
            } else {
                ctx.reply(&format!("no command named !{name}")).await
            }
        })
        .description("remove a dynamic command: !delcmd !name")
        .category("moderation")
        .check(|ctx| async move {
            ctx.state::<Moderators>()
                .map(|m| m.contains(ctx.user().name.as_str()))
                .unwrap_or(false)
        })
        .on_check_fail(|ctx| async move {
            ctx.reply("only the streamer can remove commands").await
        }),
    );

    bot.add_command(
        Command::new("cmds", |ctx: Context| async move {
            let dc = ctx
                .state::<DynamicCommands>()
                .expect("DynamicCommands not registered");
            let names = dc.list();
            if names.is_empty() {
                ctx.reply("no custom commands yet").await
            } else {
                ctx.reply(&format!("custom commands: !{}", names.join(", !")))
                    .await
            }
        })
        .description("list dynamic commands")
        .category("general"),
    );

    bot.help_command();

    // ---------------------------------------------------------------------------
    // Dynamic command dispatch (on_raw_message catch-all)
    // ---------------------------------------------------------------------------

    bot.on_raw_message(|ctx: Context| async move {
        let dc = ctx
            .state::<DynamicCommands>()
            .expect("DynamicCommands not registered");
        if let Some(reply) = dc.try_dispatch(&ctx.message, &ctx).await {
            ctx.reply(&reply).await?;
        }
        Ok(())
    });

    bot.on_error(|e| {
        tracing::error!(error = %e.error, "handler error");
    });

    bot.add_adapter(adapter);

    // Start reminder background task
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            reminder_mgr.check_due().await;
        }
    });

    // Start web dashboard
    let web_db = database.clone();
    tokio::spawn(async move {
        if let Err(e) = web::serve(web_db).await {
            tracing::error!("web dashboard error: {e}");
        }
    });

    bot.run().await.expect("bot exited with error");
}
