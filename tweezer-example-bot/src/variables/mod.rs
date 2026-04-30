mod builtins;
mod count;
mod dynamic;
mod quotes;
mod store;

pub use dynamic::DynamicCommands;
pub use quotes::QuotesDb;

use async_trait::async_trait;
use regex::Regex;
use std::sync::Arc;
use tweezer::Context;

use crate::db::Database;
use builtins::BuiltinProvider;
use count::CountProvider;

// ---------------------------------------------------------------------------
// VariableProvider trait
// ---------------------------------------------------------------------------

/// Resolves a single template variable. Return `None` if this provider does
/// not handle the given variable name as the expander will try the next one.
#[async_trait]
pub trait VariableProvider: Send + Sync {
    async fn resolve(
        &self,
        name: &str,
        args: &str,
        ctx: &Context,
        cmd_name: &str,
    ) -> Option<String>;
}

// ---------------------------------------------------------------------------
// Expander
// ---------------------------------------------------------------------------

pub struct Expander {
    providers: Vec<Box<dyn VariableProvider>>,
}

impl Expander {
    pub fn new() -> Self {
        let count = Arc::new(CountProvider::new());
        Self {
            providers: vec![Box::new(BuiltinProvider), Box::new(count)],
        }
    }

    pub fn with_db(db: Database) -> Self {
        let count = Arc::new(CountProvider::new().with_db(db));
        Self {
            providers: vec![Box::new(BuiltinProvider), Box::new(count)],
        }
    }

    /// Expand all `$(variable)` and `$(variable args)` tokens in `template`.
    /// Unknown variables are left unchanged.
    pub async fn expand(&self, template: &str, ctx: &Context, cmd_name: &str) -> String {
        let re = Regex::new(r"\$\(([^)]+)\)").unwrap();

        // Collect matches first (can't do async inside a closure passed to replace_all).
        let mut result = template.to_string();
        let matches: Vec<(String, usize, usize)> = re
            .find_iter(template)
            .map(|m| (m.as_str().to_string(), m.start(), m.end()))
            .collect();

        // Resolve in reverse order so byte offsets stay valid as we replace.
        let mut replacements: Vec<(usize, usize, String)> = Vec::new();
        for (token, start, end) in &matches {
            let inner = &token[2..token.len() - 1]; // strip $( and )
            let (name, args) = match inner.find(' ') {
                Some(i) => (&inner[..i], &inner[i + 1..]),
                None => (inner.as_ref(), ""),
            };

            let mut resolved = None;
            for provider in &self.providers {
                if let Some(val) = provider.resolve(name, args, ctx, cmd_name).await {
                    resolved = Some(val);
                    break;
                }
            }

            replacements.push((*start, *end, resolved.unwrap_or_else(|| token.clone())));
        }

        // Apply in reverse order (largest start offset first).
        replacements.sort_by(|a, b| b.0.cmp(&a.0));
        for (start, end, replacement) in replacements {
            result.replace_range(start..end, &replacement);
        }

        result
    }
}

impl Default for Expander {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tweezer::{OutgoingMessage, TypeMap, User};

    use super::*;

    fn make_ctx(
        message: &str,
        user: &str,
        channel: &str,
        platform: &str,
        args: Vec<String>,
    ) -> Context {
        let (tx, _) = mpsc::channel::<OutgoingMessage>(1);
        Context::new(
            message.to_string(),
            User {
                name: user.to_string(),
                id: "0".to_string(),
                display_name: None,
            },
            platform.to_string(),
            channel.to_string(),
            tx,
            Arc::new(|n| n.to_string()),
            Arc::new(TypeMap::new()),
            None,
        )
        .with_args(args)
    }

    fn make_ctx_with_display(user: &str, display: &str) -> Context {
        let (tx, _) = mpsc::channel::<OutgoingMessage>(1);
        Context::new(
            "".to_string(),
            User {
                name: user.to_string(),
                id: "0".to_string(),
                display_name: Some(display.to_string()),
            },
            "test".to_string(),
            "channel".to_string(),
            tx,
            Arc::new(|n| n.to_string()),
            Arc::new(TypeMap::new()),
            None,
        )
    }

    #[tokio::test]
    async fn expand_user_variable() {
        let expander = Expander::new();
        let ctx = make_ctx("", "alice", "ch", "test", vec![]);
        let out = expander.expand("hello $(user)!", &ctx, "greet").await;
        assert_eq!(out, "hello alice!");
    }

    #[tokio::test]
    async fn expand_user_prefers_display_name() {
        let expander = Expander::new();
        let ctx = make_ctx_with_display("did:plc:abc", "alice.bsky.social");
        let out = expander.expand("$(user)", &ctx, "cmd").await;
        assert_eq!(out, "alice.bsky.social");
    }

    #[tokio::test]
    async fn expand_channel_variable() {
        let expander = Expander::new();
        let ctx = make_ctx("", "alice", "mychannel", "test", vec![]);
        let out = expander.expand("channel: $(channel)", &ctx, "cmd").await;
        assert_eq!(out, "channel: mychannel");
    }

    #[tokio::test]
    async fn expand_platform_variable() {
        let expander = Expander::new();
        let ctx = make_ctx("", "alice", "ch", "streamplace", vec![]);
        let out = expander.expand("on $(platform)", &ctx, "cmd").await;
        assert_eq!(out, "on streamplace");
    }

    #[tokio::test]
    async fn expand_numbered_args() {
        let expander = Expander::new();
        let ctx = make_ctx("", "alice", "ch", "test", vec!["foo".into(), "bar".into()]);
        assert_eq!(expander.expand("$(1) $(2)", &ctx, "cmd").await, "foo bar");
    }

    #[tokio::test]
    async fn expand_arg_out_of_bounds_is_empty() {
        let expander = Expander::new();
        let ctx = make_ctx("", "alice", "ch", "test", vec!["foo".into()]);
        assert_eq!(expander.expand("$(2)", &ctx, "cmd").await, "");
    }

    #[tokio::test]
    async fn expand_touser_with_arg() {
        let expander = Expander::new();
        let ctx = make_ctx("", "alice", "ch", "test", vec!["bob".into()]);
        assert_eq!(expander.expand("$(touser)", &ctx, "cmd").await, "bob");
    }

    #[tokio::test]
    async fn expand_touser_without_arg_falls_back_to_user() {
        let expander = Expander::new();
        let ctx = make_ctx("", "alice", "ch", "test", vec![]);
        assert_eq!(expander.expand("$(touser)", &ctx, "cmd").await, "alice");
    }

    #[tokio::test]
    async fn expand_random_in_range() {
        let expander = Expander::new();
        let ctx = make_ctx("", "alice", "ch", "test", vec![]);
        for _ in 0..20 {
            let out = expander.expand("$(random 1-10)", &ctx, "cmd").await;
            let n: i64 = out.parse().expect("should be a number");
            assert!((1..=10).contains(&n));
        }
    }

    #[tokio::test]
    async fn expand_unknown_variable_unchanged() {
        let expander = Expander::new();
        let ctx = make_ctx("", "alice", "ch", "test", vec![]);
        let out = expander.expand("$(notreal)", &ctx, "cmd").await;
        assert_eq!(out, "$(notreal)");
    }

    #[tokio::test]
    async fn expand_multiple_variables() {
        let expander = Expander::new();
        let ctx = make_ctx("", "alice", "mychan", "test", vec![]);
        let out = expander.expand("$(user) in $(channel)", &ctx, "cmd").await;
        assert_eq!(out, "alice in mychan");
    }

    #[tokio::test]
    async fn expand_no_variables_unchanged() {
        let expander = Expander::new();
        let ctx = make_ctx("", "alice", "ch", "test", vec![]);
        let out = expander.expand("just plain text", &ctx, "cmd").await;
        assert_eq!(out, "just plain text");
    }
}
