use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use rocketman::{
    connection::JetstreamConnection,
    endpoints::JetstreamEndpoints,
    handler::{self, Ingestors},
    ingestion::LexiconIngestor,
    options::JetstreamOptions,
    types::event::{Event, Operation},
};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{error, info};
use tweezer::{Adapter, BotTx, IncomingMessage, OutgoingMessage, TriggerEvent, TriggerKind, TweezerError, User};

use crate::xrpc::{OutgoingFacet, UserCache, XrpcClient, resolve_pds};

const COLLECTION: &str = "place.stream.chat.message";
const FOLLOW_COLLECTION: &str = "app.bsky.graph.follow";
const TELEPORT_COLLECTION: &str = "place.stream.live.teleport";

#[derive(Deserialize)]
struct Facet {
    index: ByteSlice,
    features: Vec<FacetFeature>,
}

#[derive(Deserialize)]
struct ByteSlice {
    #[serde(rename = "byteStart")]
    byte_start: usize,
    #[serde(rename = "byteEnd")]
    byte_end: usize,
}

#[derive(Deserialize)]
struct FacetFeature {
    #[serde(rename = "$type")]
    type_: String,
    did: Option<String>,
    #[allow(dead_code)]
    uri: Option<String>,
}

enum ScannedFacet {
    Mention { byte_start: usize, byte_end: usize, handle: String },
    Link { byte_start: usize, byte_end: usize },
    BareLink { byte_start: usize, byte_end: usize },
}

fn scan_text_facets(text: &str) -> Vec<ScannedFacet> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut facets = Vec::new();
    let mut i = 0;

    while i < len {
        if bytes[i] == b'@' {
            let start = i;
            let handle_start = i + 1;
            let mut j = handle_start;
            while j < len && is_handle_byte(bytes[j]) {
                j += 1;
            }
            if j > handle_start {
                let handle_bytes = &bytes[handle_start..j];
                if handle_bytes.contains(&b'.')
                    && handle_bytes.last().is_some_and(|b| b.is_ascii_alphanumeric())
                {
                    facets.push(ScannedFacet::Mention {
                        byte_start: start,
                        byte_end: j,
                        handle: text[handle_start..j].to_string(),
                    });
                    i = j;
                    continue;
                }
            }
            i += 1;
        } else if bytes[i] == b'h' && text.is_char_boundary(i)
            && (text[i..].starts_with("https://") || text[i..].starts_with("http://"))
        {
            let start = i;
            let scheme_len = if text[i..].starts_with("https://") { 8 } else { 7 };
            let mut j = i + scheme_len;
            while j < len && !bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j > start + scheme_len {
                facets.push(ScannedFacet::Link {
                    byte_start: start,
                    byte_end: j,
                });
                i = j;
                continue;
            }
            i += 1;
        } else if bytes[i].is_ascii_alphabetic() {
            let start = i;
            let mut j = i;
            while j < len && is_domain_byte(bytes[j]) {
                j += 1;
            }
            let mut end = j;
            if end > start && bytes.get(end.wrapping_sub(1)) == Some(&b'.') {
                end -= 1;
            }
            if end <= start {
                i += 1;
                continue;
            }
            let domain_part = &text[start..end];
            let slash_pos = domain_part.find('/').unwrap_or(domain_part.len());
            let host = &domain_part[..slash_pos];
            if let Some(dot_pos) = host.find('.') {
                let tld = &host[dot_pos + 1..];
                if tld.len() >= 2 && tld.bytes().all(|b| b.is_ascii_alphabetic()) {
                    facets.push(ScannedFacet::BareLink {
                        byte_start: start,
                        byte_end: end,
                    });
                    i = end;
                    continue;
                }
            }
            i += 1;
        } else {
            i += 1;
        }
    }

    facets
}

fn is_handle_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_'
}

fn is_domain_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'/' | b'_' | b'~' | b'%' | b'?' | b'#' | b'&' | b'=' | b'+')
}

struct SharedState {
    bot_tx: BotTx,
    streamers: HashSet<String>,
    xrpc: Arc<XrpcClient>,
    user_cache: UserCache,
}

pub struct StreamplaceAdapter {
    jetstream_url: String,
    streamers: HashSet<String>,
    identifier: String,
    password: String,
}

impl StreamplaceAdapter {
    pub fn new(
        jetstream_url: impl Into<String>,
        identifier: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            jetstream_url: jetstream_url.into(),
            streamers: HashSet::new(),
            identifier: identifier.into(),
            password: password.into(),
        }
    }

    pub fn add_streamer(&mut self, did: impl Into<String>) {
        self.streamers.insert(did.into());
    }

    pub fn add_streamers(&mut self, dids: impl IntoIterator<Item = impl Into<String>>) {
        for did in dids {
            self.streamers.insert(did.into());
        }
    }
}

#[async_trait]
impl Adapter for StreamplaceAdapter {
    fn platform_name(&self) -> &str {
        "streamplace"
    }

    fn emote_fn(&self) -> std::sync::Arc<dyn Fn(&str) -> String + Send + Sync> {
        Arc::new(|name| format!(":{}:", name))
    }

    async fn connect(&mut self, bot: BotTx) -> Result<(), TweezerError> {
        if self.streamers.is_empty() {
            return Err(TweezerError::Connection(
                "at least one streamer is required".into(),
            ));
        }
        if self.identifier.is_empty() || self.password.is_empty() {
            return Err(TweezerError::Connection(
                "identifier and password are required".into(),
            ));
        }

        let pds_url = resolve_pds(&self.identifier)
            .await
            .map_err(TweezerError::Connection)?;

        let xrpc = XrpcClient::create(pds_url, self.identifier.clone(), self.password.clone())
            .await
            .map_err(TweezerError::Connection)?;

        let xrpc = Arc::new(xrpc);
        let emote_fn = self.emote_fn();

        let shared = Arc::new(SharedState {
            bot_tx: bot.clone(),
            streamers: self.streamers.clone(),
            xrpc: xrpc.clone(),
            user_cache: UserCache::new(),
        });

        let opts = JetstreamOptions::builder()
            .ws_url(JetstreamEndpoints::Custom(self.jetstream_url.clone()))
            .wanted_collections(vec![
                COLLECTION.to_string(),
                FOLLOW_COLLECTION.to_string(),
                TELEPORT_COLLECTION.to_string(),
            ])
            .build();

        let jetstream = JetstreamConnection::new(opts);
        let msg_rx = jetstream.get_msg_rx();
        let reconnect_tx = jetstream.get_reconnect_tx();
        let cursor: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));

        let follow_shared = Arc::new(SharedState {
            bot_tx: bot.clone(),
            streamers: self.streamers.clone(),
            xrpc: xrpc.clone(),
            user_cache: UserCache::new(),
        });

        let teleport_shared = Arc::new(SharedState {
            bot_tx: bot.clone(),
            streamers: self.streamers.clone(),
            xrpc: xrpc.clone(),
            user_cache: UserCache::new(),
        });

        let mut ingestors = Ingestors::new();
        ingestors.commits.insert(
            COLLECTION.to_string(),
            Box::new(ChatMessageIngestor { shared, emote_fn }),
        );
        ingestors.commits.insert(
            FOLLOW_COLLECTION.to_string(),
            Box::new(FollowIngestor { shared: follow_shared }),
        );
        ingestors.commits.insert(
            TELEPORT_COLLECTION.to_string(),
            Box::new(TeleportIngestor { shared: teleport_shared }),
        );

        let c_cursor = cursor.clone();
        tokio::spawn(async move {
            while let Ok(message) = msg_rx.recv().await {
                if let Err(e) = handler::handle_message(
                    message,
                    &ingestors,
                    reconnect_tx.clone(),
                    c_cursor.clone(),
                )
                .await
                {
                    error!("jetstream handler error: {e}");
                }
            }
        });

        let cursor_connect = cursor.clone();
        tokio::spawn(async move {
            if let Err(e) = jetstream.connect(cursor_connect).await {
                error!("jetstream connection failed: {e}");
            }
        });

        info!(
            jetstream = %self.jetstream_url,
            streamers = ?self.streamers,
            did = %xrpc.did(),
            "connected to streamplace"
        );

        Ok(())
    }
}

struct ChatMessageIngestor {
    shared: Arc<SharedState>,
    emote_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
}

#[async_trait]
impl LexiconIngestor for ChatMessageIngestor {
    async fn ingest(&self, message: Event<serde_json::Value>) -> anyhow::Result<()> {
        let commit = match &message.commit {
            Some(c) => c,
            None => return Ok(()),
        };

        if !matches!(commit.operation, Operation::Create) {
            return Ok(());
        }

        let record = match &commit.record {
            Some(r) => r,
            None => return Ok(()),
        };

        let streamer = match record.get("streamer").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return Ok(()),
        };

        if !self.shared.streamers.contains(streamer) {
            return Ok(());
        }

        let mut text = match record.get("text").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return Ok(()),
        };

        let facets: Vec<Facet> = record
            .get("facets")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        if !facets.is_empty() {
            let mut sorted = facets;
            sorted.sort_by(|a, b| b.index.byte_start.cmp(&a.index.byte_start));

            for facet in sorted {
                for feature in &facet.features {
                    match feature.type_.as_str() {
                        "app.bsky.richtext.facet#mention" => {
                            if let Some(did) = &feature.did {
                                if let Some(handle) =
                                    self.shared.user_cache.resolve_handle(did).await
                                {
                                    if let Some(replaced) = replace_byte_range(
                                        &text,
                                        facet.index.byte_start,
                                        facet.index.byte_end,
                                        &format!("@{handle}"),
                                    ) {
                                        text = replaced;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        let did = message.did.clone();
        let display_name = self.shared.user_cache.resolve_handle(&did).await;
        info!(did = %did, streamer, text = %text, "received chat message");
        let streamer_for_reply = streamer.to_string();
        let xrpc = self.shared.xrpc.clone();
        let user_cache = self.shared.user_cache.clone();

        let (reply_tx, mut reply_rx) = mpsc::channel::<OutgoingMessage>(8);

        tokio::spawn(async move {
            while let Some(msg) = reply_rx.recv().await {
                let facets = build_outgoing_facets(&msg.text, &user_cache).await;
                if let Err(e) = xrpc.send_chat_message(&msg.text, &streamer_for_reply, facets).await {
                    error!("failed to send reply: {e}");
                }
            }
        });

        let event = tweezer::Event::Message(
            IncomingMessage::new(
                "streamplace",
                User {
                    name: did.clone(),
                    id: did,
                    display_name,
                },
                text,
                streamer,
                reply_tx,
                self.emote_fn.clone(),
            )
            .max_reply_graphemes(300),
        );

        self.shared.bot_tx.send(event).await.ok();

        Ok(())
    }
}

struct FollowIngestor {
    shared: Arc<SharedState>,
}

#[async_trait]
impl LexiconIngestor for FollowIngestor {
    async fn ingest(&self, message: Event<serde_json::Value>) -> anyhow::Result<()> {
        let commit = match &message.commit {
            Some(c) => c,
            None => return Ok(()),
        };

        if !matches!(commit.operation, Operation::Create) {
            return Ok(());
        }

        let record = match &commit.record {
            Some(r) => r,
            None => return Ok(()),
        };

        let subject = match record.get("subject").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return Ok(()),
        };

        if !self.shared.streamers.contains(subject) {
            return Ok(());
        }

        let follower_did = message.did.clone();
        let display_name = self.shared.user_cache.resolve_handle(&follower_did).await;
        info!(follower = %follower_did, streamer = subject, "follow event");

        let (reply_tx, _) = mpsc::channel::<OutgoingMessage>(1);

        let event = tweezer::Event::Trigger(
            TriggerEvent::new(
                "streamplace",
                subject,
                TriggerKind::Follow {
                    user: User {
                        name: follower_did.clone(),
                        id: follower_did,
                        display_name,
                    },
                },
                reply_tx,
                Arc::new(|name| format!(":{}:", name)),
            ),
        );

        self.shared.bot_tx.send(event).await.ok();

        Ok(())
    }
}

struct TeleportIngestor {
    shared: Arc<SharedState>,
}

#[async_trait]
impl LexiconIngestor for TeleportIngestor {
    async fn ingest(&self, message: Event<serde_json::Value>) -> anyhow::Result<()> {
        let commit = match &message.commit {
            Some(c) => c,
            None => return Ok(()),
        };

        if !matches!(commit.operation, Operation::Create) {
            return Ok(());
        }

        let record = match &commit.record {
            Some(r) => r,
            None => return Ok(()),
        };

        let streamer = match record.get("streamer").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return Ok(()),
        };

        if !self.shared.streamers.contains(streamer) {
            return Ok(());
        }

        let raider_did = message.did.clone();
        let _display_name = self.shared.user_cache.resolve_handle(&raider_did).await;
        info!(raider = %raider_did, streamer, "teleport/raid event");

        let (reply_tx, _) = mpsc::channel::<OutgoingMessage>(1);

        let event = tweezer::Event::Trigger(
            TriggerEvent::new(
                "streamplace",
                streamer,
                TriggerKind::Raid {
                    from_channel: raider_did.clone(),
                    viewer_count: None,
                },
                reply_tx,
                Arc::new(|name| format!(":{}:", name)),
            ),
        );

        self.shared.bot_tx.send(event).await.ok();

        Ok(())
    }
}

async fn build_outgoing_facets(text: &str, cache: &UserCache) -> Option<Vec<OutgoingFacet>> {
    let scanned = scan_text_facets(text);
    if scanned.is_empty() {
        return None;
    }

    let mut facets = Vec::with_capacity(scanned.len());
    for s in scanned {
        match s {
            ScannedFacet::Mention { byte_start, byte_end, handle } => {
                if let Some(did) = cache.resolve_did(&handle).await {
                    facets.push(OutgoingFacet::mention(byte_start, byte_end, did));
                }
            }
            ScannedFacet::Link { byte_start, byte_end } => {
                let url = text[byte_start..byte_end].to_string();
                facets.push(OutgoingFacet::link(byte_start, byte_end, url));
            }
            ScannedFacet::BareLink { byte_start, byte_end } => {
                let domain = &text[byte_start..byte_end];
                let url = format!("https://{}", domain);
                facets.push(OutgoingFacet::link(byte_start, byte_end, url));
            }
        }
    }

    if facets.is_empty() {
        None
    } else {
        Some(facets)
    }
}

fn replace_byte_range(s: &str, start: usize, end: usize, replacement: &str) -> Option<String> {
    if start > end || end > s.len() || !s.is_char_boundary(start) || !s.is_char_boundary(end) {
        return None;
    }
    let mut result =
        String::with_capacity(s.len() - (end - start) + replacement.len());
    result.push_str(&s[..start]);
    result.push_str(replacement);
    result.push_str(&s[end..]);
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_new_stores_fields() {
        let adapter =
            StreamplaceAdapter::new("wss://jetstream.example.com", "bot@example.com", "secret");
        assert_eq!(adapter.jetstream_url, "wss://jetstream.example.com");
        assert_eq!(adapter.identifier, "bot@example.com");
        assert_eq!(adapter.password, "secret");
        assert!(adapter.streamers.is_empty());
    }

    #[test]
    fn add_streamer_inserts_did() {
        let mut adapter = StreamplaceAdapter::new("ws://x", "bot", "pw");
        adapter.add_streamer("did:plc:a");
        assert!(adapter.streamers.contains("did:plc:a"));
        assert!(!adapter.streamers.contains("did:plc:b"));
    }

    #[test]
    fn add_streamers_bulk() {
        let mut adapter = StreamplaceAdapter::new("ws://x", "bot", "pw");
        adapter.add_streamers(["did:plc:a", "did:plc:b", "did:plc:c"]);
        assert_eq!(adapter.streamers.len(), 3);
    }

    #[test]
    fn add_streamer_deduplicates() {
        let mut adapter = StreamplaceAdapter::new("ws://x", "bot", "pw");
        adapter.add_streamer("did:plc:a");
        adapter.add_streamer("did:plc:a");
        assert_eq!(adapter.streamers.len(), 1);
    }

    #[tokio::test]
    async fn connect_rejects_empty_streamers() {
        let mut adapter = StreamplaceAdapter::new("ws://x", "bot", "pw");
        let (tx, _) = mpsc::channel(1);
        let err = adapter.connect(tx).await.unwrap_err();
        assert!(matches!(err, TweezerError::Connection(_)));
        assert!(err.to_string().contains("streamer"));
    }

    #[tokio::test]
    async fn connect_rejects_empty_identifier() {
        let mut adapter = StreamplaceAdapter::new("ws://x", "", "pw");
        adapter.add_streamer("did:plc:a");
        let (tx, _) = mpsc::channel(1);
        let err = adapter.connect(tx).await.unwrap_err();
        assert!(matches!(err, TweezerError::Connection(_)));
        assert!(err.to_string().contains("identifier"));
    }

    #[tokio::test]
    async fn connect_rejects_empty_password() {
        let mut adapter = StreamplaceAdapter::new("ws://x", "bot", "");
        adapter.add_streamer("did:plc:a");
        let (tx, _) = mpsc::channel(1);
        let err = adapter.connect(tx).await.unwrap_err();
        assert!(matches!(err, TweezerError::Connection(_)));
        assert!(err.to_string().contains("password"));
    }

    #[test]
    fn platform_name() {
        let adapter = StreamplaceAdapter::new("ws://x", "bot", "pw");
        assert_eq!(adapter.platform_name(), "streamplace");
    }

    #[test]
    fn replace_byte_range_basic() {
        let result = replace_byte_range("hello world", 0, 5, "goodbye").unwrap();
        assert_eq!(result, "goodbye world");
    }

    #[test]
    fn replace_byte_range_middle() {
        let result = replace_byte_range("hello world", 6, 11, "earth").unwrap();
        assert_eq!(result, "hello earth");
    }

    #[test]
    fn replace_byte_range_end() {
        let result = replace_byte_range("hello world", 5, 11, "!").unwrap();
        assert_eq!(result, "hello!");
    }

    #[test]
    fn replace_byte_range_invalid_boundaries() {
        assert!(replace_byte_range("hello", 0, 100, "x").is_none());
        assert!(replace_byte_range("hello", 3, 2, "x").is_none());
    }

    #[test]
    fn replace_byte_range_utf8_multibyte() {
        let s = "hi 🌍 there";
        let emoji_start = s.find('🌍').unwrap();
        let emoji_end = emoji_start + "🌍".len();
        assert!(s.is_char_boundary(emoji_start));
        assert!(s.is_char_boundary(emoji_end));
        let result = replace_byte_range(s, emoji_start, emoji_end, "world").unwrap();
        assert_eq!(result, "hi world there");
    }

    #[test]
    fn replace_byte_range_mid_char_returns_none() {
        let s = "hï";
        assert_eq!(s.as_bytes().len(), 3);
        assert!(replace_byte_range(s, 1, 2, "x").is_none());
    }

    #[test]
    fn scan_finds_mention() {
        let facets = scan_text_facets("hello @alice.bsky.social world");
        assert_eq!(facets.len(), 1);
        match &facets[0] {
            ScannedFacet::Mention { byte_start, byte_end, handle } => {
                assert_eq!(*byte_start, 6);
                assert_eq!(*byte_end, 24);
                assert_eq!(handle, "alice.bsky.social");
            }
            _ => panic!("expected mention"),
        }
    }

    #[test]
    fn scan_finds_url() {
        let facets = scan_text_facets("check https://example.com out");
        assert_eq!(facets.len(), 1);
        match &facets[0] {
            ScannedFacet::Link { byte_start, byte_end } => {
                assert_eq!(*byte_start, 6);
                assert_eq!(*byte_end, 25);
            }
            _ => panic!("expected link"),
        }
    }

    #[test]
    fn scan_finds_mention_and_url() {
        let facets = scan_text_facets("@github.com https://github.com");
        assert_eq!(facets.len(), 2);
        match &facets[0] {
            ScannedFacet::Mention { handle, .. } => {
                assert_eq!(handle, "github.com");
            }
            _ => panic!("expected mention"),
        }
        match &facets[1] {
            ScannedFacet::Link { byte_start, .. } => {
                assert_eq!(*byte_start, 12);
            }
            _ => panic!("expected link"),
        }
    }

    #[test]
    fn scan_finds_bare_link() {
        let facets = scan_text_facets("check github.com out");
        assert_eq!(facets.len(), 1);
        match &facets[0] {
            ScannedFacet::BareLink { byte_start, byte_end } => {
                assert_eq!(*byte_start, 6);
                assert_eq!(*byte_end, 16);
            }
            _ => panic!("expected bare link"),
        }
    }

    #[test]
    fn scan_bare_link_with_path() {
        let facets = scan_text_facets("see docs.rs/some-crate");
        assert_eq!(facets.len(), 1);
        match &facets[0] {
            ScannedFacet::BareLink { byte_start, byte_end } => {
                assert_eq!(*byte_start, 4);
                assert_eq!(*byte_end, 22);
            }
            _ => panic!("expected bare link"),
        }
    }

    #[test]
    fn scan_mention_and_bare_link() {
        let facets = scan_text_facets("@github.com github.com");
        assert_eq!(facets.len(), 2);
        match &facets[0] {
            ScannedFacet::Mention { .. } => {}
            _ => panic!("expected mention"),
        }
        match &facets[1] {
            ScannedFacet::BareLink { .. } => {}
            _ => panic!("expected bare link"),
        }
    }

    #[test]
    fn scan_rejects_no_dot() {
        let facets = scan_text_facets("@alice world");
        assert!(facets.is_empty());
    }

    #[test]
    fn scan_rejects_trailing_dot() {
        let facets = scan_text_facets("@alice. world");
        assert!(facets.is_empty());
    }

    #[test]
    fn scan_handles_utf8_text() {
        let text = "hi @alice.bsky.social 你好";
        let facets = scan_text_facets(text);
        assert_eq!(facets.len(), 1);
        match &facets[0] {
            ScannedFacet::Mention { byte_start, byte_end, handle } => {
                assert_eq!(*byte_start, 3);
                assert_eq!(*byte_end, 21);
                assert_eq!(handle, "alice.bsky.social");
            }
            _ => panic!("expected mention"),
        }
    }

    #[test]
    fn scan_http_url() {
        let facets = scan_text_facets("see http://example.com");
        assert_eq!(facets.len(), 1);
        match &facets[0] {
            ScannedFacet::Link { byte_start, byte_end } => {
                assert_eq!(*byte_start, 4);
                assert_eq!(*byte_end, 22);
            }
            _ => panic!("expected link"),
        }
    }

    #[test]
    fn scan_empty_text() {
        assert!(scan_text_facets("").is_empty());
    }
}
