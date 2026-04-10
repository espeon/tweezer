mod adapter;
mod bot;
mod context;
mod error;
mod event;
mod message;
mod test_util;
pub mod trigger;
mod typemap;
mod user;

pub mod test {
    pub use crate::test_util::TestContextBuilder;
}

pub use adapter::{Adapter, BotTx};
pub use bot::{Bot, Command, HandlerError, HandlerErrorKind, HelpEntry, ShutdownHandle};
pub use context::Context;
pub use error::TweezerError;
pub use event::{Event, IncomingMessage, LifecycleEvent, LifecycleKind};
pub use message::OutgoingMessage;
pub use trigger::{
    PlatformTrigger, TriggerContext, TriggerEvent, TriggerKind,
};
pub use typemap::TypeMap;
pub use user::User;

pub mod prelude {
    pub use crate::{
        Adapter, Bot, BotTx, Context, PlatformTrigger, TriggerContext, TriggerKind, TweezerError,
    };
    pub use async_trait::async_trait;
}
