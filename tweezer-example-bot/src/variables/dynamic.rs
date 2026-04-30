use std::sync::Arc;

use tweezer::Context;

use super::Expander;
use crate::db::Database;

#[derive(Clone)]
pub struct DynamicCommands {
    db: Option<Database>,
    expander: Arc<Expander>,
}

impl DynamicCommands {
    pub fn new(expander: Arc<Expander>) -> Self {
        Self {
            db: None,
            expander,
        }
    }

    pub fn with_db(mut self, db: Database) -> Self {
        self.db = Some(db);
        self
    }

    pub fn add(&self, name: &str, template: &str) {
        if let Some(ref db) = self.db {
            if let Err(e) = db.cmd_add(name, template) {
                tracing::warn!("failed to persist command: {e}");
            }
        }
    }

    pub fn remove(&self, name: &str) -> bool {
        if let Some(ref db) = self.db {
            match db.cmd_remove(name) {
                Ok(removed) => removed,
                Err(e) => {
                    tracing::warn!("failed to remove command: {e}");
                    false
                }
            }
        } else {
            false
        }
    }

    pub fn get(&self, name: &str) -> Option<String> {
        self.db.as_ref()?.cmd_get(name).unwrap_or(None)
    }

    pub fn list(&self) -> Vec<String> {
        match self.db {
            Some(ref db) => db.cmd_list().unwrap_or_default().into_iter().map(|(n, _)| n).collect(),
            None => Vec::new(),
        }
    }

    pub async fn try_dispatch(&self, message: &str, ctx: &Context) -> Option<String> {
        let rest = message.strip_prefix('!')?;
        let cmd_name = rest.split_whitespace().next()?;
        let template = self.get(cmd_name)?;
        Some(self.expander.expand(&template, ctx, cmd_name).await)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tweezer::{OutgoingMessage, TypeMap, User};

    use super::*;
    use crate::db::Database;

    fn make_ctx(user: &str) -> Context {
        let (tx, _) = mpsc::channel::<OutgoingMessage>(1);
        Context::new(
            "".to_string(),
            User { name: user.to_string(), id: "0".to_string(), display_name: None, color: None, labels: Vec::new(), badges: Vec::new() },
            "test".to_string(),
            "channel".to_string(),
            tx,
            Arc::new(|n| n.to_string()),
            Arc::new(TypeMap::new()),
            None,
        )
    }

    fn make_dc() -> DynamicCommands {
        let db = Database::open_in_memory().unwrap();
        DynamicCommands::new(Arc::new(Expander::new())).with_db(db)
    }

    #[test]
    fn add_and_get_command() {
        let dc = make_dc();
        dc.add("hello", "Hello, $(user)!");
        assert_eq!(dc.get("hello"), Some("Hello, $(user)!".into()));
    }

    #[test]
    fn remove_command() {
        let dc = make_dc();
        dc.add("hello", "hi");
        assert!(dc.remove("hello"));
        assert!(dc.get("hello").is_none());
    }

    #[test]
    fn remove_nonexistent_returns_false() {
        let dc = make_dc();
        assert!(!dc.remove("nope"));
    }

    #[test]
    fn get_unknown_returns_none() {
        let dc = make_dc();
        assert!(dc.get("unknown").is_none());
    }

    #[test]
    fn list_returns_sorted_names() {
        let dc = make_dc();
        dc.add("zebra", "z");
        dc.add("alpha", "a");
        dc.add("middle", "m");
        assert_eq!(dc.list(), vec!["alpha", "middle", "zebra"]);
    }

    #[tokio::test]
    async fn try_dispatch_expands_template() {
        let dc = make_dc();
        dc.add("greet", "Hi $(user)!");
        let ctx = make_ctx("alice");
        let result = dc.try_dispatch("!greet", &ctx).await;
        assert_eq!(result, Some("Hi alice!".into()));
    }

    #[tokio::test]
    async fn try_dispatch_returns_none_for_unknown_command() {
        let dc = make_dc();
        let ctx = make_ctx("alice");
        assert!(dc.try_dispatch("!unknown", &ctx).await.is_none());
    }

    #[tokio::test]
    async fn try_dispatch_returns_none_for_non_command() {
        let dc = make_dc();
        dc.add("hello", "hi");
        let ctx = make_ctx("alice");
        assert!(dc.try_dispatch("hello", &ctx).await.is_none());
    }
}
