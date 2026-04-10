use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use tweezer::Context;

use super::{Expander, store::CommandStore};

#[derive(Clone)]
pub struct DynamicCommands {
    commands: Arc<RwLock<HashMap<String, String>>>,
    expander: Arc<Expander>,
    store: Option<Arc<dyn CommandStore>>,
}

impl DynamicCommands {
    pub fn new(expander: Arc<Expander>) -> Self {
        Self {
            commands: Arc::new(RwLock::new(HashMap::new())),
            expander,
            store: None,
        }
    }

    pub fn with_store(mut self, store: impl CommandStore + 'static) -> Self {
        let store = Arc::new(store);
        if let Ok(commands) = store.load() {
            *self.commands.write().unwrap() = commands;
        }
        self.store = Some(store);
        self
    }

    pub fn add(&self, name: &str, template: &str) {
        self.commands.write().unwrap().insert(name.to_string(), template.to_string());
        self.persist();
    }

    pub fn remove(&self, name: &str) -> bool {
        let removed = self.commands.write().unwrap().remove(name).is_some();
        if removed {
            self.persist();
        }
        removed
    }

    pub fn get(&self, name: &str) -> Option<String> {
        self.commands.read().unwrap().get(name).cloned()
    }

    pub fn list(&self) -> Vec<String> {
        let map = self.commands.read().unwrap();
        let mut names: Vec<String> = map.keys().cloned().collect();
        names.sort();
        names
    }

    pub async fn try_dispatch(&self, message: &str, ctx: &Context) -> Option<String> {
        let rest = message.strip_prefix('!')?;
        let cmd_name = rest.split_whitespace().next()?;
        let template = self.get(cmd_name)?;
        Some(self.expander.expand(&template, ctx, cmd_name).await)
    }

    fn persist(&self) {
        if let Some(ref store) = self.store {
            let commands = self.commands.read().unwrap();
            if let Err(e) = store.save(&commands) {
                tracing::warn!("failed to persist commands: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tweezer::{OutgoingMessage, TypeMap, User};

    use super::*;

    fn make_ctx(user: &str) -> Context {
        let (tx, _) = mpsc::channel::<OutgoingMessage>(1);
        Context::new(
            "".to_string(),
            User { name: user.to_string(), id: "0".to_string(), display_name: None },
            "test".to_string(),
            "channel".to_string(),
            tx,
            Arc::new(|n| n.to_string()),
            Arc::new(TypeMap::new()),
            None,
        )
    }

    fn make_dc() -> DynamicCommands {
        DynamicCommands::new(Arc::new(Expander::new()))
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
