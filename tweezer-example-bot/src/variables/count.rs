use std::sync::Arc;

use async_trait::async_trait;
use tweezer::Context;

use super::VariableProvider;
use crate::db::Database;

pub struct CountProvider {
    db: Option<Database>,
}

impl CountProvider {
    pub fn new() -> Self {
        Self { db: None }
    }

    pub fn with_db(mut self, db: Database) -> Self {
        self.db = Some(db);
        self
    }
}

#[async_trait]
impl VariableProvider for Arc<CountProvider> {
    async fn resolve(&self, name: &str, _args: &str, ctx: &Context, cmd_name: &str) -> Option<String> {
        if name != "count" {
            return None;
        }
        let key = format!("{}:{}", ctx.channel(), cmd_name);
        let count = match self.db {
            Some(ref db) => db.counter_increment(&key).unwrap_or(0),
            None => {
                // Fallback to in-memory only
                return Some("1".into());
            }
        };
        Some(count.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tweezer::{OutgoingMessage, TypeMap, User};

    use super::*;
    use crate::db::Database;

    fn make_ctx(channel: &str) -> Context {
        let (tx, _) = mpsc::channel::<OutgoingMessage>(1);
        Context::new(
            "".to_string(),
            User { name: "alice".to_string(), id: "0".to_string(), display_name: None, color: None, labels: Vec::new(), badges: Vec::new() },
            "test".to_string(),
            channel.to_string(),
            tx,
            Arc::new(|n| n.to_string()),
            Arc::new(TypeMap::new()),
            None,
        )
    }

    fn make_provider() -> Arc<CountProvider> {
        let db = Database::open_in_memory().unwrap();
        Arc::new(CountProvider::new().with_db(db))
    }

    #[tokio::test]
    async fn count_increments_per_key() {
        let provider = make_provider();
        let ctx = make_ctx("mychan");
        assert_eq!(provider.resolve("count", "", &ctx, "so").await, Some("1".into()));
        assert_eq!(provider.resolve("count", "", &ctx, "so").await, Some("2".into()));
        assert_eq!(provider.resolve("count", "", &ctx, "so").await, Some("3".into()));
    }

    #[tokio::test]
    async fn count_is_independent_per_channel() {
        let provider = make_provider();
        let ctx_a = make_ctx("chan-a");
        let ctx_b = make_ctx("chan-b");
        assert_eq!(provider.resolve("count", "", &ctx_a, "so").await, Some("1".into()));
        assert_eq!(provider.resolve("count", "", &ctx_b, "so").await, Some("1".into()));
        assert_eq!(provider.resolve("count", "", &ctx_a, "so").await, Some("2".into()));
    }

    #[tokio::test]
    async fn count_is_independent_per_command() {
        let provider = make_provider();
        let ctx = make_ctx("chan");
        assert_eq!(provider.resolve("count", "", &ctx, "cmd1").await, Some("1".into()));
        assert_eq!(provider.resolve("count", "", &ctx, "cmd2").await, Some("1".into()));
    }

    #[tokio::test]
    async fn non_count_variable_returns_none() {
        let provider = make_provider();
        let ctx = make_ctx("chan");
        assert!(provider.resolve("user", "", &ctx, "cmd").await.is_none());
    }
}
