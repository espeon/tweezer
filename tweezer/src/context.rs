use std::{future::Future, pin::Pin, sync::Arc};

use tokio::sync::mpsc::Sender;
use unicode_segmentation::UnicodeSegmentation;

use crate::{typemap::TypeMap, FromArgs, OutgoingMessage, ParseArgsError, ReplyRef, TweezerError, User};

type DeleteFn = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), TweezerError>> + Send>> + Send + Sync,
>;

/// Platform-agnostic moderation action.
#[derive(Debug, Clone)]
pub enum ModerationAction {
    HideMessage { message_uri: String },
    UnhideMessage { gate_uri: String },
    BanUser { user_did: String },
    UnbanUser { block_uri: String },
    PinMessage { message_uri: String, expires_at: Option<String> },
    UnpinMessage { pin_uri: String },
}

type ModerationFn = Arc<
    dyn Fn(ModerationAction) -> Pin<Box<dyn Future<Output = Result<(), TweezerError>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct Context {
    pub message: String,
    pub user: User,
    platform: String,
    channel: String,
    args: Vec<String>,
    reply_tx: Sender<OutgoingMessage>,
    emote_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
    state: Arc<TypeMap>,
    max_reply_graphemes: Option<usize>,
    message_id: Option<String>,
    delete_fn: Option<DeleteFn>,
    moderation_fn: Option<ModerationFn>,
    reply: Option<ReplyRef>,
    is_streamer: bool,
    is_moderator: bool,
}

impl Context {
    pub fn new(
        message: String,
        user: User,
        platform: String,
        channel: String,
        reply_tx: Sender<OutgoingMessage>,
        emote_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
        state: Arc<TypeMap>,
        max_reply_graphemes: Option<usize>,
    ) -> Self {
        Self {
            args: Vec::new(),
            message,
            user,
            platform,
            channel,
            reply_tx,
            emote_fn,
            state,
            max_reply_graphemes,
            message_id: None,
            delete_fn: None,
            moderation_fn: None,
            reply: None,
            is_streamer: false,
            is_moderator: false,
        }
    }

    pub(crate) fn with_delete(mut self, delete_fn: Option<DeleteFn>) -> Self {
        self.delete_fn = delete_fn;
        self
    }

    pub(crate) fn with_message_id(mut self, id: Option<String>) -> Self {
        self.message_id = id;
        self
    }

    pub(crate) fn with_moderation(mut self, moderation_fn: Option<ModerationFn>) -> Self {
        self.moderation_fn = moderation_fn;
        self
    }

    pub(crate) fn with_reply(mut self, reply: Option<ReplyRef>) -> Self {
        self.reply = reply;
        self
    }

    pub(crate) fn with_streamer(mut self, yes: bool) -> Self {
        self.is_streamer = yes;
        self
    }

    pub(crate) fn with_moderator(mut self, yes: bool) -> Self {
        self.is_moderator = yes;
        self
    }

    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    pub fn platform(&self) -> &str {
        &self.platform
    }

    pub fn channel(&self) -> &str {
        &self.channel
    }

    pub fn user(&self) -> &User {
        &self.user
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Platform-specific message identifier, if the adapter provides one.
    pub fn message_id(&self) -> Option<&str> {
        self.message_id.as_deref()
    }

    /// Reply reference if this message is a reply to another message.
    pub fn reply_to(&self) -> Option<&ReplyRef> {
        self.reply.as_ref()
    }

    /// True if the message author is the streamer for this channel.
    pub fn is_streamer(&self) -> bool {
        self.is_streamer
    }

    /// True if the message author has moderation permissions for this channel.
    pub fn is_moderator(&self) -> bool {
        self.is_moderator
    }

    /// Delete the original message that triggered this command.
    /// Returns `Ok(())` even if the adapter does not support deletion.
    pub async fn delete(&self) -> Result<(), TweezerError> {
        if let Some(ref delete_fn) = self.delete_fn {
            delete_fn().await
        } else {
            Ok(())
        }
    }

    /// Perform a moderation action. Returns `Ok(())` if the adapter does not support moderation.
    pub async fn moderate(&self, action: ModerationAction) -> Result<(), TweezerError> {
        if let Some(ref moderation_fn) = self.moderation_fn {
            moderation_fn(action).await
        } else {
            Ok(())
        }
    }

    /// Parse arguments into typed values.
    ///
    /// Supports primitives, `String`, `Option<T>`, tuples, and `Vec<T>`.
    /// Returns an error if parsing fails or if there are unconsumed arguments.
    ///
    /// # Example
    /// ```rust,ignore
    /// let (icao, plain): (String, bool) = ctx.parse_args()?;
    /// ```
    pub fn parse_args<T: FromArgs>(&self) -> Result<T, ParseArgsError> {
        let (val, rest) = T::from_args(&self.args)?;
        if !rest.is_empty() {
            return Err(ParseArgsError::TooManyArguments);
        }
        Ok(val)
    }

    /// Look up a value previously registered with `Bot::register`.
    /// Returns `None` if nothing was registered for type `T`.
    pub fn state<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.state.get::<T>()
    }

    pub fn emote(&self, name: &str) -> String {
        (self.emote_fn)(name)
    }

    pub async fn reply(&self, text: &str) -> Result<(), TweezerError> {
        let chunks = match self.max_reply_graphemes {
            Some(max) => split_grapheme_chunks(text, max),
            None => vec![text.to_string()],
        };
        for chunk in chunks {
            self.reply_tx
                .send(OutgoingMessage { text: chunk })
                .await
                .map_err(|e| TweezerError::Reply(e.to_string()))?;
        }
        Ok(())
    }
}

pub(crate) fn split_grapheme_chunks(text: &str, max: usize) -> Vec<String> {
    let graphemes: Vec<&str> = UnicodeSegmentation::graphemes(text, true).collect();
    if graphemes.len() <= max {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut start = 0;
    while start < graphemes.len() {
        let end = (start + max).min(graphemes.len());

        if end < graphemes.len() {
            let slice = &graphemes[start..end];
            let last_space = slice.iter().rposition(|g| g.contains(' '));
            let split_at = match last_space {
                Some(i) => start + i + 1,
                None => end,
            };
            chunks.push(graphemes[start..split_at].concat());
            start = split_at;
        } else {
            chunks.push(graphemes[start..end].concat());
            start = end;
        }
    }

    chunks
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use crate::{OutgoingMessage, TweezerError, User};

    use super::*;

    fn make_ctx() -> (Context, mpsc::Receiver<OutgoingMessage>) {
        let (tx, rx) = mpsc::channel(8);
        let ctx = Context::new(
            "hello world".into(),
            User { name: "alice".into(), id: "1".into(), display_name: None, color: None, labels: Vec::new(), badges: Vec::new() },
            "test".into(),
            "general".into(),
            tx,
            Arc::new(|name| format!(":{}:", name)),
            Arc::new(TypeMap::new()),
            None,
        );
        (ctx, rx)
    }

    fn make_ctx_with_limit(limit: usize) -> (Context, mpsc::Receiver<OutgoingMessage>) {
        let (tx, rx) = mpsc::channel(8);
        let ctx = Context::new(
            "hello world".into(),
            User { name: "alice".into(), id: "1".into(), display_name: None, color: None, labels: Vec::new(), badges: Vec::new() },
            "test".into(),
            "general".into(),
            tx,
            Arc::new(|name| format!(":{}:", name)),
            Arc::new(TypeMap::new()),
            Some(limit),
        );
        (ctx, rx)
    }

    #[test]
    fn context_accessors() {
        let (ctx, _) = make_ctx();
        assert_eq!(ctx.message, "hello world");
        assert_eq!(ctx.user().name, "alice");
        assert_eq!(ctx.user().id, "1");
        assert_eq!(ctx.platform(), "test");
        assert_eq!(ctx.channel(), "general");
    }

    #[test]
    fn context_emote() {
        let (ctx, _) = make_ctx();
        assert_eq!(ctx.emote("pog"), ":pog:");
    }

    #[tokio::test]
    async fn context_reply_sends_message() {
        let (ctx, mut rx) = make_ctx();
        ctx.reply("pong").await.unwrap();
        let msg = rx.try_recv().unwrap();
        assert_eq!(msg.text, "pong");
    }

    #[tokio::test]
    async fn context_reply_error_when_dropped() {
        let (ctx, rx) = make_ctx();
        drop(rx);
        let err = ctx.reply("should fail").await.unwrap_err();
        assert!(matches!(err, TweezerError::Reply(_)));
    }

    #[test]
    fn context_is_cloneable() {
        let (ctx, _) = make_ctx();
        let ctx2 = ctx.clone();
        assert_eq!(ctx2.message, ctx.message);
        assert_eq!(ctx2.channel(), ctx.channel());
    }

    #[tokio::test]
    async fn reply_no_split_when_under_limit() {
        let (ctx, mut rx) = make_ctx_with_limit(50);
        ctx.reply("short message").await.unwrap();
        let msg = rx.try_recv().unwrap();
        assert_eq!(msg.text, "short message");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn reply_splits_at_word_boundary() {
        let (ctx, mut rx) = make_ctx_with_limit(12);
        ctx.reply("hello world foo bar").await.unwrap();
        let msg1 = rx.try_recv().unwrap();
        let msg2 = rx.try_recv().unwrap();
        assert_eq!(msg1.text, "hello world ");
        assert_eq!(msg2.text, "foo bar");
    }

    #[tokio::test]
    async fn reply_splits_long_word_no_spaces() {
        let (ctx, mut rx) = make_ctx_with_limit(5);
        ctx.reply("abcdefghij").await.unwrap();
        let msg1 = rx.try_recv().unwrap();
        let msg2 = rx.try_recv().unwrap();
        assert_eq!(msg1.text, "abcde");
        assert_eq!(msg2.text, "fghij");
    }

    #[tokio::test]
    async fn reply_no_limit_sends_single() {
        let (ctx, mut rx) = make_ctx();
        ctx.reply("a very long message that would otherwise need splitting").await.unwrap();
        let msg = rx.try_recv().unwrap();
        assert_eq!(msg.text, "a very long message that would otherwise need splitting");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn split_grapheme_chunks_respects_limit() {
        let chunks = split_grapheme_chunks("aaa bbb ccc ddd", 8);
        for chunk in &chunks {
            assert!(UnicodeSegmentation::graphemes(chunk.as_str(), true).count() <= 8);
        }
    }

    #[test]
    fn split_grapheme_chunks_unicode() {
        let text = "na\u{00EF}ve people are great";
        let chunks = split_grapheme_chunks(text, 10);
        for chunk in &chunks {
            assert!(UnicodeSegmentation::graphemes(chunk.as_str(), true).count() <= 10);
        }
        assert_eq!(chunks.join(""), text);
    }

    #[test]
    fn parse_args_basic() {
        let (ctx, _) = make_ctx_with_args(&["hello", "42"]);
        let (name, count): (String, u32) = ctx.parse_args().unwrap();
        assert_eq!(name, "hello");
        assert_eq!(count, 42);
    }

    #[test]
    fn parse_args_with_option_some() {
        let (ctx, _) = make_ctx_with_args(&["hello", "42"]);
        let (name, opt): (String, Option<u32>) = ctx.parse_args().unwrap();
        assert_eq!(name, "hello");
        assert_eq!(opt, Some(42));
    }

    #[test]
    fn parse_args_with_option_none() {
        let (ctx, _) = make_ctx_with_args(&["hello"]);
        let (name, opt): (String, Option<u32>) = ctx.parse_args().unwrap();
        assert_eq!(name, "hello");
        assert_eq!(opt, None);
    }

    #[test]
    fn parse_args_too_many() {
        let (ctx, _) = make_ctx_with_args(&["a", "b", "c"]);
        let result: Result<(String, String), _> = ctx.parse_args();
        assert_eq!(result.unwrap_err(), ParseArgsError::TooManyArguments);
    }

    #[test]
    fn parse_args_invalid_type() {
        let (ctx, _) = make_ctx_with_args(&["abc"]);
        let result: Result<u32, _> = ctx.parse_args();
        assert!(
            matches!(result, Err(ParseArgsError::InvalidValue { expected: "u32", .. })),
            "got {result:?}"
        );
    }

    #[test]
    fn parse_args_missing() {
        let (ctx, _) = make_ctx_with_args(&[]);
        let result: Result<String, _> = ctx.parse_args();
        assert_eq!(result.unwrap_err(), ParseArgsError::MissingArgument(0));
    }

    #[test]
    fn parse_args_vec_consumes_rest() {
        let (ctx, _) = make_ctx_with_args(&["a", "b", "c"]);
        let vals: Vec<String> = ctx.parse_args().unwrap();
        assert_eq!(vals, vec!["a", "b", "c"]);
    }

    #[test]
    fn parse_args_tuple_with_vec() {
        let (ctx, _) = make_ctx_with_args(&["cmd", "1", "2", "3"]);
        let (cmd, nums): (String, Vec<u32>) = ctx.parse_args().unwrap();
        assert_eq!(cmd, "cmd");
        assert_eq!(nums, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn delete_calls_callback_when_set() {
        let deleted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let d = deleted.clone();
        let (tx, rx) = mpsc::channel(8);
        let ctx = Context::new(
            "hello world".into(),
            User { name: "alice".into(), id: "1".into(), display_name: None, color: None, labels: Vec::new(), badges: Vec::new() },
            "test".into(),
            "general".into(),
            tx,
            Arc::new(|name| format!(":{}:", name)),
            Arc::new(TypeMap::new()),
            None,
        )
        .with_delete(Some(Arc::new(move || {
            let d = d.clone();
            Box::pin(async move {
                d.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        })));
        ctx.delete().await.unwrap();
        assert!(deleted.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn delete_returns_ok_when_no_callback() {
        let (tx, rx) = mpsc::channel(8);
        let ctx = Context::new(
            "hello world".into(),
            User { name: "alice".into(), id: "1".into(), display_name: None, color: None, labels: Vec::new(), badges: Vec::new() },
            "test".into(),
            "general".into(),
            tx,
            Arc::new(|name| format!(":{}:", name)),
            Arc::new(TypeMap::new()),
            None,
        );
        ctx.delete().await.unwrap();
    }

    #[tokio::test]
    async fn delete_propagates_error() {
        let (tx, rx) = mpsc::channel(8);
        let ctx = Context::new(
            "hello world".into(),
            User { name: "alice".into(), id: "1".into(), display_name: None, color: None, labels: Vec::new(), badges: Vec::new() },
            "test".into(),
            "general".into(),
            tx,
            Arc::new(|name| format!(":{}:", name)),
            Arc::new(TypeMap::new()),
            None,
        )
        .with_delete(Some(Arc::new(|| {
            Box::pin(async { Err(TweezerError::Handler("nope".into())) })
        })));
        let err = ctx.delete().await.unwrap_err();
        assert!(matches!(err, TweezerError::Handler(_)));
    }

    #[test]
    fn message_id_accessor() {
        let (tx, rx) = mpsc::channel(8);
        let ctx = Context::new(
            "hello world".into(),
            User { name: "alice".into(), id: "1".into(), display_name: None, color: None, labels: Vec::new(), badges: Vec::new() },
            "test".into(),
            "general".into(),
            tx,
            Arc::new(|name| format!(":{}:", name)),
            Arc::new(TypeMap::new()),
            None,
        )
        .with_message_id(Some("msg-42".into()));
        assert_eq!(ctx.message_id(), Some("msg-42"));
    }

    fn make_ctx_with_args(args: &[&str]) -> (Context, mpsc::Receiver<OutgoingMessage>) {
        let (tx, rx) = mpsc::channel(8);
        let ctx = Context::new(
            "hello world".into(),
            User { name: "alice".into(), id: "1".into(), display_name: None, color: None, labels: Vec::new(), badges: Vec::new() },
            "test".into(),
            "general".into(),
            tx,
            Arc::new(|name| format!(":{}:", name)),
            Arc::new(TypeMap::new()),
            None,
        )
        .with_args(args.iter().map(|s| s.to_string()).collect());
        (ctx, rx)
    }
}
