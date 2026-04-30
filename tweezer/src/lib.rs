mod adapter;
mod args;
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
    pub use crate::test_util::{TestContextBuilder, TestTriggerContextBuilder};
}

pub use adapter::{Adapter, BotTx};
pub use args::{FromArgs, ParseArgsError};
pub use bot::{Bot, Command, HandlerError, HandlerErrorKind, HelpEntry, IntoCommand, RateLimitStrategy, ShutdownHandle};
pub use tweezer_macros::command;
pub use context::Context;
pub use error::TweezerError;
pub use event::{Event, IncomingMessage, LifecycleEvent, LifecycleKind, ModerationAction, ReplyRef};
pub use message::OutgoingMessage;
pub use trigger::{
    PlatformTrigger, TriggerContext, TriggerEvent, TriggerKind,
};
pub use typemap::TypeMap;
pub use user::{BadgeInfo, BadgeKind, ChatColor, User};

pub mod prelude {
    pub use crate::{
        Adapter, BadgeInfo, BadgeKind, Bot, BotTx, ChatColor, Command, Context, FromArgs,
        HelpEntry, IntoCommand, ModerationAction, ParseArgsError, PlatformTrigger,
        RateLimitStrategy, ReplyRef, TriggerContext, TriggerKind, TweezerError,
    };
    pub use async_trait::async_trait;
    pub use tweezer_macros::command;
}
