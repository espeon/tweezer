//! Test utilities for consumers building bot tests.
//!
//! ```
//! use tweezer::test::TestContextBuilder;
//!
//! let (ctx, _rx) = TestContextBuilder::new("alice", "hello")
//!     .platform("streamplace")
//!     .channel("my-stream")
//!     .build();
//! ```

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::{Context, OutgoingMessage, TypeMap, User};

pub struct TestContextBuilder {
    user_name: String,
    user_id: String,
    display_name: Option<String>,
    message: String,
    platform: String,
    channel: String,
    args: Vec<String>,
    max_reply_graphemes: Option<usize>,
}

impl TestContextBuilder {
    pub fn new(user: &str, message: &str) -> Self {
        Self {
            user_name: user.to_string(),
            user_id: user.to_string(),
            display_name: None,
            message: message.to_string(),
            platform: "test".to_string(),
            channel: "test-channel".to_string(),
            args: Vec::new(),
            max_reply_graphemes: None,
        }
    }

    pub fn user_id(mut self, id: impl Into<String>) -> Self {
        self.user_id = id.into();
        self
    }

    pub fn display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    pub fn platform(mut self, p: impl Into<String>) -> Self {
        self.platform = p.into();
        self
    }

    pub fn channel(mut self, ch: impl Into<String>) -> Self {
        self.channel = ch.into();
        self
    }

    pub fn args(mut self, args: Vec<&str>) -> Self {
        self.args = args.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn max_reply_graphemes(mut self, n: usize) -> Self {
        self.max_reply_graphemes = Some(n);
        self
    }

    pub fn build(self) -> (Context, mpsc::Receiver<OutgoingMessage>) {
        let (tx, rx) = mpsc::channel(8);
        let ctx = Context::new(
            self.message,
            User {
                name: self.user_name,
                id: self.user_id,
                display_name: self.display_name,
            },
            self.platform,
            self.channel,
            tx,
            Arc::new(|n| n.to_string()),
            Arc::new(TypeMap::new()),
            self.max_reply_graphemes,
        )
        .with_args(self.args);
        (ctx, rx)
    }
}
