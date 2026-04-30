//! Test utilities for consumers building bot tests.
//!
//! ```
//! use tweezer::test::TestContextBuilder;
//!
//! let (ctx, _rx, _deleted) = TestContextBuilder::new("alice", "hello")
//!     .platform("streamplace")
//!     .channel("my-stream")
//!     .build();
//! ```

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::{
    Context, OutgoingMessage, TweezerError, TypeMap, User,
    trigger::{TriggerContext, TriggerEvent, TriggerKind},
};

pub struct TestContextBuilder {
    user_name: String,
    user_id: String,
    display_name: Option<String>,
    message: String,
    platform: String,
    channel: String,
    args: Vec<String>,
    max_reply_graphemes: Option<usize>,
    state: TypeMap,
    message_id: Option<String>,
    delete_should_fail: bool,
}

impl TestContextBuilder {
    pub fn new(user: &str, message: &str) -> Self {
        Self {
            user_name: user.to_string(),
            user_id: user.to_string(),
            display_name: None,
            message: message.to_string(),
            platform: "test".to_string(),
            channel: "test-channel".to_string(),
            args: Vec::new(),
            max_reply_graphemes: None,
            state: TypeMap::new(),
            message_id: None,
            delete_should_fail: false,
        }
    }

    pub fn user_id(mut self, id: impl Into<String>) -> Self {
        self.user_id = id.into();
        self
    }

    pub fn display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    pub fn platform(mut self, p: impl Into<String>) -> Self {
        self.platform = p.into();
        self
    }

    pub fn channel(mut self, ch: impl Into<String>) -> Self {
        self.channel = ch.into();
        self
    }

    pub fn args(mut self, args: Vec<&str>) -> Self {
        self.args = args.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn max_reply_graphemes(mut self, n: usize) -> Self {
        self.max_reply_graphemes = Some(n);
        self
    }

    pub fn state<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        self.state.insert(value);
        self
    }

    pub fn message_id(mut self, id: impl Into<String>) -> Self {
        self.message_id = Some(id.into());
        self
    }

    pub fn delete_should_fail(mut self) -> Self {
        self.delete_should_fail = true;
        self
    }

    pub fn build(self) -> (Context, mpsc::Receiver<OutgoingMessage>, Arc<std::sync::atomic::AtomicBool>) {
        let (tx, rx) = mpsc::channel(8);
        let deleted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let deleted_clone = deleted.clone();
        let should_fail = self.delete_should_fail;
        let delete_fn = Arc::new(move || {
            let deleted = deleted_clone.clone();
            let should_fail = should_fail;
            Box::pin(async move {
                if should_fail {
                    Err(TweezerError::Handler("delete failed".into()))
                } else {
                    deleted.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                }
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), TweezerError>> + Send>>
        });
        let ctx = Context::new(
            self.message,
            User {
                name: self.user_name,
                id: self.user_id,
                display_name: self.display_name,
            },
            self.platform,
            self.channel,
            tx,
            Arc::new(|n: &str| n.to_string()),
            Arc::new(self.state),
            self.max_reply_graphemes,
        )
        .with_args(self.args)
        .with_message_id(self.message_id)
        .with_delete(Some(delete_fn));
        (ctx, rx, deleted)
    }
}

pub struct TestTriggerContextBuilder {
    kind: TriggerKind,
    platform: String,
    channel: String,
    max_reply_graphemes: Option<usize>,
    state: TypeMap,
}

impl TestTriggerContextBuilder {
    pub fn new(kind: TriggerKind) -> Self {
        Self {
            kind,
            platform: "test".to_string(),
            channel: "test-channel".to_string(),
            max_reply_graphemes: None,
            state: TypeMap::new(),
        }
    }

    pub fn platform(mut self, p: impl Into<String>) -> Self {
        self.platform = p.into();
        self
    }

    pub fn channel(mut self, ch: impl Into<String>) -> Self {
        self.channel = ch.into();
        self
    }

    pub fn max_reply_graphemes(mut self, n: usize) -> Self {
        self.max_reply_graphemes = Some(n);
        self
    }

    pub fn state<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        self.state.insert(value);
        self
    }

    pub fn build(self) -> (TriggerContext, mpsc::Receiver<OutgoingMessage>) {
        let (tx, rx) = mpsc::channel(8);
        let mut event = TriggerEvent::new(
            self.platform,
            self.channel,
            self.kind,
            tx,
            Arc::new(|n: &str| n.to_string()),
        );
        if let Some(max) = self.max_reply_graphemes {
            event = event.max_reply_graphemes(max);
        }
        let ctx = TriggerContext::new(event, Arc::new(self.state));
        (ctx, rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::User;

    #[test]
    fn context_builder_defaults() {
        let (ctx, _, _) = TestContextBuilder::new("alice", "hello").build();
        assert_eq!(ctx.user().name, "alice");
        assert_eq!(ctx.message, "hello");
        assert_eq!(ctx.platform(), "test");
        assert_eq!(ctx.channel(), "test-channel");
    }

    #[test]
    fn context_builder_state_injection() {
        struct Counter(u32);
        let (ctx, _, _) = TestContextBuilder::new("alice", "hello")
            .state(Counter(42))
            .build();
        assert_eq!(ctx.state::<Counter>().unwrap().0, 42);
    }

    #[test]
    fn context_builder_multiple_state_types() {
        struct Name(String);
        struct Count(u32);
        let (ctx, _, _) = TestContextBuilder::new("alice", "hello")
            .state(Name("foo".into()))
            .state(Count(7))
            .build();
        assert_eq!(ctx.state::<Name>().unwrap().0, "foo");
        assert_eq!(ctx.state::<Count>().unwrap().0, 7);
    }

    #[test]
    fn context_builder_missing_state_returns_none() {
        struct Missing;
        let (ctx, _, _) = TestContextBuilder::new("alice", "hello").build();
        assert!(ctx.state::<Missing>().is_none());
    }

    #[test]
    fn trigger_context_builder_defaults() {
        let kind = TriggerKind::Raid {
            from_channel: "friend".into(),
            viewer_count: Some(10),
        };
        let (ctx, _) = TestTriggerContextBuilder::new(kind).build();
        assert_eq!(ctx.platform(), "test");
        assert_eq!(ctx.channel(), "test-channel");
    }

    #[test]
    fn trigger_context_builder_fields() {
        let kind = TriggerKind::Raid {
            from_channel: "friend".into(),
            viewer_count: None,
        };
        let (ctx, _) = TestTriggerContextBuilder::new(kind)
            .platform("streamplace")
            .channel("my-stream")
            .build();
        assert_eq!(ctx.platform(), "streamplace");
        assert_eq!(ctx.channel(), "my-stream");
    }

    #[tokio::test]
    async fn trigger_context_builder_reply() {
        let kind = TriggerKind::Follow {
            user: User { name: "alice".into(), id: "1".into(), display_name: None },
        };
        let (ctx, mut rx) = TestTriggerContextBuilder::new(kind).build();
        ctx.reply("welcome!").await.unwrap();
        assert_eq!(rx.try_recv().unwrap().text, "welcome!");
    }

    #[test]
    fn trigger_context_builder_state_injection() {
        struct Greeting(String);
        let kind = TriggerKind::Raid {
            from_channel: "friend".into(),
            viewer_count: Some(5),
        };
        let (ctx, _) = TestTriggerContextBuilder::new(kind)
            .state(Greeting("welcome".into()))
            .build();
        assert_eq!(ctx.state::<Greeting>().unwrap().0, "welcome");
    }

    #[test]
    fn trigger_context_builder_kind_accessible() {
        let kind = TriggerKind::Follow {
            user: User { name: "bob".into(), id: "2".into(), display_name: None },
        };
        let (ctx, _) = TestTriggerContextBuilder::new(kind).build();
        assert!(matches!(ctx.kind, TriggerKind::Follow { .. }));
    }
}
