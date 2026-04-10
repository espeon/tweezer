use std::sync::Arc;

use tokio::sync::mpsc::Sender;
use unicode_segmentation::UnicodeSegmentation;

use crate::{typemap::TypeMap, OutgoingMessage, TweezerError, User};

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
        }
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
            User { name: "alice".into(), id: "1".into(), display_name: None },
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
            User { name: "alice".into(), id: "1".into(), display_name: None },
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
}
