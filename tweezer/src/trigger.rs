use std::sync::Arc;

use tokio::sync::mpsc::Sender;

use crate::context::split_grapheme_chunks;
use crate::{typemap::TypeMap, OutgoingMessage, TweezerError, User};

#[derive(Debug, Clone)]
pub enum TriggerKind {
    Raid {
        from_channel: String,
        viewer_count: Option<u32>,
    },
    Follow {
        user: User,
    },
    Subscription {
        user: User,
        tier: String,
        months: u32,
        message: Option<String>,
    },
    Donation {
        user: User,
        amount_cents: u64,
        currency: String,
        message: Option<String>,
    },
    MessageHidden {
        message_uri: String,
        hidden_by: String,
    },
    UserBanned {
        user: User,
        banned_by: String,
    },
    UserUnbanned {
        user_did: String,
        unbanned_by: String,
    },
    MessagePinned {
        message_uri: String,
        pinned_by: String,
        expires_at: Option<String>,
    },
    MessageUnpinned {
        unpinned_by: String,
    },
    Platform(Box<dyn PlatformTrigger>),
}

pub trait PlatformTrigger: Send + Sync + std::fmt::Debug {
    fn kind_id(&self) -> &str;
    fn as_any(&self) -> &dyn std::any::Any;
    fn clone_box(&self) -> Box<dyn PlatformTrigger>;
}

impl Clone for Box<dyn PlatformTrigger> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

pub struct TriggerEvent {
    pub(crate) platform: String,
    pub(crate) channel: String,
    pub(crate) kind: TriggerKind,
    pub(crate) reply_tx: Sender<OutgoingMessage>,
    pub(crate) emote_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub(crate) max_reply_graphemes: Option<usize>,
}

impl TriggerEvent {
    pub fn new(
        platform: impl Into<String>,
        channel: impl Into<String>,
        kind: TriggerKind,
        reply_tx: Sender<OutgoingMessage>,
        emote_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
    ) -> Self {
        Self {
            platform: platform.into(),
            channel: channel.into(),
            kind,
            reply_tx,
            emote_fn,
            max_reply_graphemes: None,
        }
    }

    pub fn max_reply_graphemes(mut self, n: usize) -> Self {
        self.max_reply_graphemes = Some(n);
        self
    }

    pub fn platform(&self) -> &str {
        &self.platform
    }

    pub fn channel(&self) -> &str {
        &self.channel
    }

    pub fn kind(&self) -> &TriggerKind {
        &self.kind
    }
}

#[derive(Clone)]
pub struct TriggerContext {
    pub kind: TriggerKind,
    platform: String,
    channel: String,
    reply_tx: Sender<OutgoingMessage>,
    emote_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
    state: Arc<TypeMap>,
    max_reply_graphemes: Option<usize>,
}

impl TriggerContext {
    pub(crate) fn new(event: TriggerEvent, state: Arc<TypeMap>) -> Self {
        Self {
            kind: event.kind,
            platform: event.platform,
            channel: event.channel,
            reply_tx: event.reply_tx,
            emote_fn: event.emote_fn,
            state,
            max_reply_graphemes: event.max_reply_graphemes,
        }
    }

    pub fn platform(&self) -> &str {
        &self.platform
    }

    pub fn channel(&self) -> &str {
        &self.channel
    }

    pub fn emote(&self, name: &str) -> String {
        (self.emote_fn)(name)
    }

    pub fn state<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.state.get::<T>()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_trigger_ctx(kind: TriggerKind) -> (TriggerContext, tokio::sync::mpsc::Receiver<OutgoingMessage>) {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let event = TriggerEvent {
            platform: "test".into(),
            channel: "general".into(),
            kind,
            reply_tx: tx,
            emote_fn: Arc::new(|name| format!(":{}:", name)),
            max_reply_graphemes: None,
        };
        (TriggerContext::new(event, Arc::new(TypeMap::new())), rx)
    }

    #[test]
    fn trigger_context_accessors() {
        let kind = TriggerKind::Raid {
            from_channel: "friend".into(),
            viewer_count: Some(10),
        };
        let (ctx, _) = make_trigger_ctx(kind);
        assert_eq!(ctx.platform(), "test");
        assert_eq!(ctx.channel(), "general");
        assert_eq!(ctx.emote("hi"), ":hi:");
    }

    #[tokio::test]
    async fn trigger_context_reply() {
        let kind = TriggerKind::Follow {
            user: User { name: "alice".into(), id: "1".into(), display_name: None, color: None, labels: Vec::new(), badges: Vec::new() },
        };
        let (ctx, mut rx) = make_trigger_ctx(kind);
        ctx.reply("welcome!").await.unwrap();
        let msg = rx.try_recv().unwrap();
        assert_eq!(msg.text, "welcome!");
    }

    #[test]
    fn trigger_context_clone() {
        let kind = TriggerKind::Raid {
            from_channel: "friend".into(),
            viewer_count: Some(5),
        };
        let (ctx, _) = make_trigger_ctx(kind);
        let ctx2 = ctx.clone();
        assert_eq!(ctx2.platform(), ctx.platform());
        assert_eq!(ctx2.channel(), ctx.channel());
    }

    #[test]
    fn platform_trigger_clone_box() {
        #[derive(Debug, Clone)]
        struct TestTrigger;
        impl PlatformTrigger for TestTrigger {
            fn kind_id(&self) -> &str { "test" }
            fn as_any(&self) -> &dyn std::any::Any { self }
            fn clone_box(&self) -> Box<dyn PlatformTrigger> { Box::new(self.clone()) }
        }
        let boxed: Box<dyn PlatformTrigger> = Box::new(TestTrigger);
        let cloned = boxed.clone();
        assert_eq!(cloned.kind_id(), "test");
    }
}
