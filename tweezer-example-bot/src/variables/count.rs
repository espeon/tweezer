use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use tweezer::Context;

use super::VariableProvider;

pub struct CountProvider {
    counts: Arc<RwLock<HashMap<String, u64>>>,
}

impl CountProvider {
    pub fn new() -> Self {
        Self { counts: Arc::new(RwLock::new(HashMap::new())) }
    }
}

#[async_trait]
impl VariableProvider for Arc<CountProvider> {
    async fn resolve(&self, name: &str, _args: &str, ctx: &Context, cmd_name: &str) -> Option<String> {
        if name != "count" {
            return None;
        }
        // Key is channel:command so counts are independent per channel.
        let key = format!("{}:{}", ctx.channel(), cmd_name);
        let mut counts = self.counts.write().unwrap();
        let entry = counts.entry(key).or_insert(0);
        *entry += 1;
        Some(entry.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tweezer::{OutgoingMessage, TypeMap, User};

    use super::*;

    fn make_ctx(channel: &str) -> Context {
        let (tx, _) = mpsc::channel::<OutgoingMessage>(1);
        Context::new(
            "".to_string(),
            User { name: "alice".to_string(), id: "0".to_string(), display_name: None },
            "test".to_string(),
            channel.to_string(),
            tx,
            Arc::new(|n| n.to_string()),
            Arc::new(TypeMap::new()),
            None,
        )
    }

    #[tokio::test]
    async fn count_increments_per_key() {
        let provider = Arc::new(CountProvider::new());
        let ctx = make_ctx("mychan");
        assert_eq!(provider.resolve("count", "", &ctx, "so").await, Some("1".into()));
        assert_eq!(provider.resolve("count", "", &ctx, "so").await, Some("2".into()));
        assert_eq!(provider.resolve("count", "", &ctx, "so").await, Some("3".into()));
    }

    #[tokio::test]
    async fn count_is_independent_per_channel() {
        let provider = Arc::new(CountProvider::new());
        let ctx_a = make_ctx("chan-a");
        let ctx_b = make_ctx("chan-b");
        assert_eq!(provider.resolve("count", "", &ctx_a, "so").await, Some("1".into()));
        assert_eq!(provider.resolve("count", "", &ctx_b, "so").await, Some("1".into()));
        assert_eq!(provider.resolve("count", "", &ctx_a, "so").await, Some("2".into()));
    }

    #[tokio::test]
    async fn count_is_independent_per_command() {
        let provider = Arc::new(CountProvider::new());
        let ctx = make_ctx("chan");
        assert_eq!(provider.resolve("count", "", &ctx, "cmd1").await, Some("1".into()));
        assert_eq!(provider.resolve("count", "", &ctx, "cmd2").await, Some("1".into()));
    }

    #[tokio::test]
    async fn non_count_variable_returns_none() {
        let provider = Arc::new(CountProvider::new());
        let ctx = make_ctx("chan");
        assert!(provider.resolve("user", "", &ctx, "cmd").await.is_none());
    }
}
