use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc::Sender;

use crate::{Event, TweezerError};

pub type BotTx = Sender<Event>;

#[async_trait]
pub trait Adapter: Send + Sync {
    fn platform_name(&self) -> &str;
    fn emote_fn(&self) -> Arc<dyn Fn(&str) -> String + Send + Sync>;
    async fn connect(&mut self, bot: BotTx) -> Result<(), TweezerError>;
}
