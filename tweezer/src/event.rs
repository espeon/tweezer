use std::sync::Arc;

use tokio::sync::mpsc::Sender;

use crate::{OutgoingMessage, User};

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

pub struct IncomingMessage {
    pub(crate) platform: String,
    pub(crate) user: User,
    pub(crate) text: String,
    pub(crate) channel: String,
    pub(crate) reply_tx: Sender<OutgoingMessage>,
    pub(crate) emote_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub(crate) max_reply_graphemes: Option<usize>,
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
        }
    }

    pub fn max_reply_graphemes(mut self, n: usize) -> Self {
        self.max_reply_graphemes = Some(n);
        self
    }
}
