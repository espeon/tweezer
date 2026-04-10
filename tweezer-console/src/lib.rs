use std::{
    any::Any,
    collections::HashMap,
    sync::Arc,
};

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tweezer::{
    Adapter, BotTx, Event, IncomingMessage, LifecycleEvent, LifecycleKind, OutgoingMessage,
    PlatformTrigger, TriggerEvent, TriggerKind, TweezerError, User,
};

// ---------------------------------------------------------------------------
// ConsolePlatformTrigger
// ---------------------------------------------------------------------------

/// A platform trigger for use in console sessions.
/// Carries arbitrary key=value fields parsed from the `/trigger platform` command.
#[derive(Debug, Clone)]
pub struct ConsolePlatformTrigger {
    pub id: String,
    pub fields: HashMap<String, String>,
}

impl PlatformTrigger for ConsolePlatformTrigger {
    fn kind_id(&self) -> &str {
        &self.id
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn clone_box(&self) -> Box<dyn PlatformTrigger> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// ConsoleAdapter
// ---------------------------------------------------------------------------

pub struct ConsoleAdapter {
    platform: String,
    username: String,
}

impl ConsoleAdapter {
    pub fn new() -> Self {
        Self { platform: "console".into(), username: "you".into() }
    }

    /// Masquerade as another platform. Useful for testing platform-specific
    /// branches in bot logic without a real connection.
    pub fn masquerade(mut self, platform: &str) -> Self {
        self.platform = platform.into();
        self
    }

    pub fn username(mut self, username: &str) -> Self {
        self.username = username.into();
        self
    }
}

impl Default for ConsoleAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for ConsoleAdapter {
    fn platform_name(&self) -> &str {
        &self.platform
    }

    fn emote_fn(&self) -> Arc<dyn Fn(&str) -> String + Send + Sync> {
        Arc::new(|name| name.to_string())
    }

    async fn connect(&mut self, bot: BotTx) -> Result<(), TweezerError> {
        let platform = self.platform.clone();
        let username = self.username.clone();
        let emote_fn = self.emote_fn();

        let _ = bot.send(Event::Lifecycle(LifecycleEvent {
            platform: platform.clone(),
            kind: LifecycleKind::Connected,
        })).await;

        tokio::spawn(async move {
            let mut lines = BufReader::new(tokio::io::stdin()).lines();

            loop {
                eprint!("[{}] {}> ", platform, username);

                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let line = line.trim().to_string();
                        if line.is_empty() {
                            continue;
                        }

                        let (reply_tx, mut reply_rx) =
                            tokio::sync::mpsc::channel::<OutgoingMessage>(8);

                        tokio::spawn(async move {
                            while let Some(msg) = reply_rx.recv().await {
                                println!("< {}", msg.text);
                            }
                        });

                        let event = if let Some(rest) = line.strip_prefix("/trigger ") {
                            match parse_trigger(rest, &platform, &username, reply_tx, &emote_fn) {
                                Ok(event) => event,
                                Err(msg) => {
                                    eprintln!("trigger parse error: {msg}");
                                    continue;
                                }
                            }
                        } else {
                            Event::Message(IncomingMessage::new(
                                platform.clone(),
                                User { name: username.clone(), id: "0".into(), display_name: None },
                                line,
                                "console",
                                reply_tx,
                                emote_fn.clone(),
                            ))
                        };

                        if bot.send(event).await.is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        });

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// /trigger parsing
// ---------------------------------------------------------------------------

/// Parses a `/trigger <kind> [key=value ...]` line into an `Event::Trigger`.
///
/// Supported kinds:
/// ```text
/// /trigger raid from=StreamFriend viewers=150
/// /trigger follow user=alice
/// /trigger sub user=bob tier=1 months=3
/// /trigger donation user=carol amount=500 currency=USD
/// /trigger platform kind=my.custom.event key=value ...
/// ```
fn parse_trigger(
    rest: &str,
    platform: &str,
    username: &str,
    reply_tx: tokio::sync::mpsc::Sender<OutgoingMessage>,
    emote_fn: &Arc<dyn Fn(&str) -> String + Send + Sync>,
) -> Result<Event, String> {
    let mut parts = rest.splitn(2, ' ');
    let kind_str = parts.next().unwrap_or("").trim();
    let args_str = parts.next().unwrap_or("").trim();

    let args = parse_kv(args_str);

    let get = |key: &str| -> Result<String, String> {
        args.get(key)
            .cloned()
            .ok_or_else(|| format!("missing required argument '{key}'"))
    };

    let user_from_args = |args: &HashMap<String, String>| -> User {
        let name = args.get("user").cloned().unwrap_or_else(|| username.to_string());
        User { name, id: "0".into(), display_name: None }
    };

    let kind = match kind_str {
        "raid" => TriggerKind::Raid {
            from_channel: get("from")?,
            viewer_count: get("viewers")
                .unwrap_or_else(|_| "0".into())
                .parse()
                .unwrap_or(0),
        },
        "follow" => TriggerKind::Follow { user: user_from_args(&args) },
        "sub" => TriggerKind::Subscription {
            user: user_from_args(&args),
            tier: args.get("tier").cloned().unwrap_or_else(|| "1".into()),
            months: get("months")
                .unwrap_or_else(|_| "1".into())
                .parse()
                .unwrap_or(1),
            message: args.get("message").cloned(),
        },
        "donation" => TriggerKind::Donation {
            user: user_from_args(&args),
            amount_cents: get("amount")
                .unwrap_or_else(|_| "0".into())
                .parse()
                .unwrap_or(0),
            currency: get("currency").unwrap_or_else(|_| "USD".into()),
            message: args.get("message").cloned(),
        },
        "platform" => {
            let id = get("kind")?;
            let mut fields = args.clone();
            fields.remove("kind");
            TriggerKind::Platform(Box::new(ConsolePlatformTrigger { id, fields }))
        }
        other => return Err(format!("unknown trigger kind '{other}'")),
    };

    Ok(Event::Trigger(TriggerEvent::new(
        platform,
        "console",
        kind,
        reply_tx,
        emote_fn.clone(),
    )))
}

/// Parses `key=value key2=value2` into a `HashMap`.
fn parse_kv(s: &str) -> HashMap<String, String> {
    s.split_whitespace()
        .filter_map(|pair| {
            let mut it = pair.splitn(2, '=');
            let k = it.next()?.to_string();
            let v = it.next()?.to_string();
            Some((k, v))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_emote_fn() -> Arc<dyn Fn(&str) -> String + Send + Sync> {
        Arc::new(|name| name.to_string())
    }

    fn test_reply_tx() -> tokio::sync::mpsc::Sender<OutgoingMessage> {
        tokio::sync::mpsc::channel(1).0
    }

    #[test]
    fn parse_kv_basic() {
        let result = parse_kv("from=StreamFriend viewers=150");
        assert_eq!(result.get("from").unwrap(), "StreamFriend");
        assert_eq!(result.get("viewers").unwrap(), "150");
    }

    #[test]
    fn parse_kv_empty() {
        let result = parse_kv("");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_kv_ignores_no_equals() {
        let result = parse_kv("foo bar=baz");
        assert!(!result.contains_key("foo"));
        assert_eq!(result.get("bar").unwrap(), "baz");
    }

    #[test]
    fn parse_kv_value_with_equals() {
        let result = parse_kv("key=a=b");
        assert_eq!(result.get("key").unwrap(), "a=b");
    }

    #[test]
    fn parse_trigger_raid() {
        let tx = test_reply_tx();
        let emote = test_emote_fn();
        let result = parse_trigger("raid from=Friend viewers=42", "test", "user", tx, &emote);
        let event = result.unwrap();
        match event {
            Event::Trigger(te) => match te.kind() {
                TriggerKind::Raid { from_channel, viewer_count } => {
                    assert_eq!(from_channel, "Friend");
                    assert_eq!(*viewer_count, 42);
                }
                other => panic!("expected Raid, got {:?}", other),
            },
            _ => panic!("expected Trigger event"),
        }
    }

    #[test]
    fn parse_trigger_raid_missing_from() {
        let tx = test_reply_tx();
        let emote = test_emote_fn();
        let result = parse_trigger("raid viewers=10", "test", "user", tx, &emote);
        let err = result.err().unwrap();
        assert!(err.contains("from"));
    }

    #[test]
    fn parse_trigger_follow_explicit_user() {
        let tx = test_reply_tx();
        let emote = test_emote_fn();
        let result = parse_trigger("follow user=alice", "test", "user", tx, &emote);
        let event = result.unwrap();
        match event {
            Event::Trigger(te) => match &te.kind() {
                TriggerKind::Follow { user } => {
                    assert_eq!(user.name, "alice");
                }
                other => panic!("expected Follow, got {:?}", other),
            },
            _ => panic!("expected Trigger event"),
        }
    }

    #[test]
    fn parse_trigger_follow_default_user() {
        let tx = test_reply_tx();
        let emote = test_emote_fn();
        let result = parse_trigger("follow", "test", "default_user", tx, &emote);
        let event = result.unwrap();
        match event {
            Event::Trigger(te) => match &te.kind() {
                TriggerKind::Follow { user } => {
                    assert_eq!(user.name, "default_user");
                }
                other => panic!("expected Follow, got {:?}", other),
            },
            _ => panic!("expected Trigger event"),
        }
    }

    #[test]
    fn parse_trigger_sub_defaults() {
        let tx = test_reply_tx();
        let emote = test_emote_fn();
        let result = parse_trigger("sub user=bob", "test", "user", tx, &emote);
        let event = result.unwrap();
        match event {
            Event::Trigger(te) => match &te.kind() {
                TriggerKind::Subscription { user, tier, months, message } => {
                    assert_eq!(user.name, "bob");
                    assert_eq!(tier, "1");
                    assert_eq!(*months, 1);
                    assert!(message.is_none());
                }
                other => panic!("expected Subscription, got {:?}", other),
            },
            _ => panic!("expected Trigger event"),
        }
    }

    #[test]
    fn parse_trigger_sub_all_fields() {
        let tx = test_reply_tx();
        let emote = test_emote_fn();
        let result = parse_trigger(
            "sub user=carol tier=prime months=12 message=hello",
            "test",
            "user",
            tx,
            &emote,
        );
        let event = result.unwrap();
        match event {
            Event::Trigger(te) => match &te.kind() {
                TriggerKind::Subscription { user, tier, months, message } => {
                    assert_eq!(user.name, "carol");
                    assert_eq!(tier, "prime");
                    assert_eq!(*months, 12);
                    assert_eq!(message.as_deref(), Some("hello"));
                }
                other => panic!("expected Subscription, got {:?}", other),
            },
            _ => panic!("expected Trigger event"),
        }
    }

    #[test]
    fn parse_trigger_sub_tier2_and_tier3() {
        let emote = test_emote_fn();

        let tx2 = test_reply_tx();
        let result2 = parse_trigger("sub tier=2", "test", "u", tx2, &emote);
        match result2.unwrap() {
            Event::Trigger(te) => match &te.kind() {
                TriggerKind::Subscription { tier, .. } => assert_eq!(tier, "2"),
                _ => panic!(),
            },
            _ => panic!(),
        }

        let tx3 = test_reply_tx();
        let result3 = parse_trigger("sub tier=3", "test", "u", tx3, &emote);
        match result3.unwrap() {
            Event::Trigger(te) => match &te.kind() {
                TriggerKind::Subscription { tier, .. } => assert_eq!(tier, "3"),
                _ => panic!(),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn parse_trigger_donation() {
        let tx = test_reply_tx();
        let emote = test_emote_fn();
        let result = parse_trigger(
            "donation user=dave amount=500 currency=EUR message=gg",
            "test",
            "user",
            tx,
            &emote,
        );
        let event = result.unwrap();
        match event {
            Event::Trigger(te) => match &te.kind() {
                TriggerKind::Donation { user, amount_cents, currency, message } => {
                    assert_eq!(user.name, "dave");
                    assert_eq!(*amount_cents, 500);
                    assert_eq!(currency, "EUR");
                    assert_eq!(message.as_deref(), Some("gg"));
                }
                other => panic!("expected Donation, got {:?}", other),
            },
            _ => panic!("expected Trigger event"),
        }
    }

    #[test]
    fn parse_trigger_donation_defaults() {
        let tx = test_reply_tx();
        let emote = test_emote_fn();
        let result = parse_trigger("donation user=eve", "test", "user", tx, &emote);
        let event = result.unwrap();
        match event {
            Event::Trigger(te) => match &te.kind() {
                TriggerKind::Donation { amount_cents, currency, message, .. } => {
                    assert_eq!(*amount_cents, 0);
                    assert_eq!(currency, "USD");
                    assert!(message.is_none());
                }
                other => panic!("expected Donation, got {:?}", other),
            },
            _ => panic!("expected Trigger event"),
        }
    }

    #[test]
    fn parse_trigger_platform() {
        let tx = test_reply_tx();
        let emote = test_emote_fn();
        let result = parse_trigger(
            "platform kind=my.custom.event foo=bar baz=qux",
            "test",
            "user",
            tx,
            &emote,
        );
        let event = result.unwrap();
        match event {
            Event::Trigger(te) => match &te.kind() {
                TriggerKind::Platform(pt) => {
                    assert_eq!(pt.kind_id(), "my.custom.event");
                    let console = pt.as_any().downcast_ref::<ConsolePlatformTrigger>().unwrap();
                    assert_eq!(console.fields.get("foo").unwrap(), "bar");
                    assert_eq!(console.fields.get("baz").unwrap(), "qux");
                    assert!(!console.fields.contains_key("kind"));
                }
                other => panic!("expected Platform, got {:?}", other),
            },
            _ => panic!("expected Trigger event"),
        }
    }

    #[test]
    fn parse_trigger_platform_missing_kind() {
        let tx = test_reply_tx();
        let emote = test_emote_fn();
        let result = parse_trigger("platform foo=bar", "test", "user", tx, &emote);
        let err = result.err().unwrap();
        assert!(err.contains("kind"));
    }

    #[test]
    fn parse_trigger_unknown_kind() {
        let tx = test_reply_tx();
        let emote = test_emote_fn();
        let result = parse_trigger("blarg", "test", "user", tx, &emote);
        let err = result.err().unwrap();
        assert!(err.contains("blarg"));
    }

    #[test]
    fn parse_trigger_sets_platform_and_channel() {
        let tx = test_reply_tx();
        let emote = test_emote_fn();
        let result = parse_trigger("follow", "myplatform", "user", tx, &emote);
        match result.unwrap() {
            Event::Trigger(te) => {
                assert_eq!(te.platform(), "myplatform");
                assert_eq!(te.channel(), "console");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn console_adapter_new() {
        let adapter = ConsoleAdapter::new();
        assert_eq!(adapter.platform_name(), "console");
    }

    #[test]
    fn console_adapter_masquerade() {
        let adapter = ConsoleAdapter::new().masquerade("twitch");
        assert_eq!(adapter.platform_name(), "twitch");
    }

    #[test]
    fn console_adapter_default() {
        let adapter = ConsoleAdapter::default();
        assert_eq!(adapter.platform_name(), "console");
    }

    #[test]
    fn console_platform_trigger_impl() {
        let trigger = ConsolePlatformTrigger {
            id: "test.trigger".into(),
            fields: {
                let mut m = HashMap::new();
                m.insert("key".into(), "value".into());
                m
            },
        };
        assert_eq!(trigger.kind_id(), "test.trigger");
        let cloned = trigger.clone_box();
        assert_eq!(cloned.kind_id(), "test.trigger");
        let any = trigger.as_any();
        let downcast = any.downcast_ref::<ConsolePlatformTrigger>().unwrap();
        assert_eq!(downcast.fields.get("key").unwrap(), "value");
    }
}
