use std::{future::Future, pin::Pin, sync::Arc};

use tokio::sync::mpsc::Sender;

use crate::{OutgoingMessage, TweezerError, User};

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

pub enum Event {
    Message(IncomingMessage),
    Trigger(crate::trigger::TriggerEvent),
    Lifecycle(LifecycleEvent),
}

pub struct LifecycleEvent {
    pub platform: String,
    pub kind: LifecycleKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleKind {
    Connected,
    Disconnected,
    Ready,
}

/// Reference to the message this reply is in response to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyRef {
    pub root_uri: String,
    pub root_cid: String,
    pub parent_uri: String,
    pub parent_cid: String,
}

type DeleteFn = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), TweezerError>> + Send>> + Send + Sync,
>;

pub struct IncomingMessage {
    pub(crate) platform: String,
    pub(crate) user: User,
    pub(crate) text: String,
    pub(crate) channel: String,
    pub(crate) reply_tx: Sender<OutgoingMessage>,
    pub(crate) emote_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub(crate) max_reply_graphemes: Option<usize>,
    pub(crate) message_id: Option<String>,
    pub(crate) delete_fn: Option<DeleteFn>,
    pub(crate) moderation_fn: Option<ModerationFn>,
    pub(crate) reply: Option<ReplyRef>,
    pub(crate) is_streamer: bool,
    pub(crate) is_moderator: bool,
}

impl IncomingMessage {
    pub fn new(
        platform: impl Into<String>,
        user: User,
        text: impl Into<String>,
        channel: impl Into<String>,
        reply_tx: Sender<OutgoingMessage>,
        emote_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
    ) -> Self {
        Self {
            platform: platform.into(),
            user,
            text: text.into(),
            channel: channel.into(),
            reply_tx,
            emote_fn,
            max_reply_graphemes: None,
            message_id: None,
            delete_fn: None,
            moderation_fn: None,
            reply: None,
            is_streamer: false,
            is_moderator: false,
        }
    }

    pub fn max_reply_graphemes(mut self, n: usize) -> Self {
        self.max_reply_graphemes = Some(n);
        self
    }

    pub fn message_id(mut self, id: impl Into<String>) -> Self {
        self.message_id = Some(id.into());
        self
    }

    pub fn on_delete<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TweezerError>> + Send + 'static,
    {
        self.delete_fn = Some(Arc::new(move || Box::pin(f())));
        self
    }

    pub fn on_moderate<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(ModerationAction) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TweezerError>> + Send + 'static,
    {
        self.moderation_fn = Some(Arc::new(move |action| Box::pin(f(action))));
        self
    }

    pub fn reply(mut self, reply: Option<ReplyRef>) -> Self {
        self.reply = reply;
        self
    }

    pub fn is_streamer(mut self, yes: bool) -> Self {
        self.is_streamer = yes;
        self
    }

    pub fn is_moderator(mut self, yes: bool) -> Self {
        self.is_moderator = yes;
        self
    }
}
