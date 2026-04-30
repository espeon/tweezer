use std::{future::Future, pin::Pin, sync::Arc};

use tokio::sync::mpsc::Sender;

use crate::{OutgoingMessage, TweezerError, User};

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
}
