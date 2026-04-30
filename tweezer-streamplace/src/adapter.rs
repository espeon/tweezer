use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use jacquard_common::{
    CowStr, RawData,
    deps::fluent_uri::{ParseError, Uri},
    jetstream::{CommitOperation, RawJetstreamMessage, RawJetstreamParams},
    xrpc::{SubscriptionClient, TungsteniteSubscriptionClient},
};
use n0_future::StreamExt;
use tokio::sync::mpsc;
use tracing::{error, info};
use tweezer::{
    Adapter, BotTx, IncomingMessage, OutgoingMessage, TriggerEvent, TriggerKind, TweezerError, User,
};

use crate::xrpc::{OutgoingFacet, UserCache, XrpcClient, resolve_pds};

const COLLECTION: &str = "place.stream.chat.message";
const FOLLOW_COLLECTION: &str = "app.bsky.graph.follow";
const TELEPORT_COLLECTION: &str = "place.stream.live.teleport";

struct Facet {
    index: ByteSlice,
    features: Vec<FacetFeature>,
}

struct ByteSlice {
    byte_start: usize,
    byte_end: usize,
}

struct FacetFeature {
    type_: String,
    did: Option<String>,
    #[allow(dead_code)]
    uri: Option<String>,
}

enum ScannedFacet {
    Mention {
        byte_start: usize,
        byte_end: usize,
        handle: String,
    },
    Link {
        byte_start: usize,
        byte_end: usize,
    },
    BareLink {
        byte_start: usize,
        byte_end: usize,
    },
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
                    && handle_bytes
                        .last()
                        .is_some_and(|b| b.is_ascii_alphanumeric())
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
        } else if bytes[i] == b'h'
            && text.is_char_boundary(i)
            && (text[i..].starts_with("https://") || text[i..].starts_with("http://"))
        {
            let start = i;
            let scheme_len = if text[i..].starts_with("https://") {
                8
            } else {
                7
            };
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
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'.' | b'-' | b'/' | b'_' | b'~' | b'%' | b'?' | b'#' | b'&' | b'=' | b'+'
        )
}

struct SharedState {
    bot_tx: BotTx,
    streamers: HashSet<String>,
    xrpc: Arc<XrpcClient>,
    user_cache: UserCache,
    emote_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
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

        let base_uri = jetstream_base_uri(&self.jetstream_url)
            .map_err(|e| TweezerError::Connection(e.to_string()))?;

        let shared = Arc::new(SharedState {
            bot_tx: bot.clone(),
            streamers: self.streamers.clone(),
            xrpc: Arc::new(xrpc),
            user_cache: UserCache::new(),
            emote_fn: self.emote_fn(),
        });

        info!(
            jetstream = %self.jetstream_url,
            streamers = ?self.streamers,
            did = %shared.xrpc.did(),
            "connected to streamplace"
        );

        tokio::spawn(async move {
            let client = TungsteniteSubscriptionClient::from_base_uri(base_uri);
            let params = RawJetstreamParams::new()
                .wanted_collections(vec![
                    CowStr::from(COLLECTION),
                    CowStr::from(FOLLOW_COLLECTION),
                    CowStr::from(TELEPORT_COLLECTION),
                ])
                .build();

            loop {
                let stream = match client.subscribe(&params).await {
                    Ok(s) => s,
                    Err(e) => {
                        error!("jetstream connect failed: {e}");
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                        continue;
                    }
                };

                let (_sink, mut messages) = stream.into_stream();

                loop {
                    match messages.next().await {
                        Some(Ok(msg)) => handle_message(msg, &shared).await,
                        Some(Err(e)) => {
                            error!("jetstream stream error: {e}");
                            break;
                        }
                        None => break,
                    }
                }

                info!("jetstream disconnected, reconnecting in 5s");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        });

        Ok(())
    }
}

fn jetstream_base_uri(url: &str) -> Result<Uri<String>, ParseError> {
    let without_scheme = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("wss://")
        .trim_start_matches("ws://");
    let host = without_scheme.split('/').next().unwrap_or(without_scheme);
    Uri::parse(format!("wss://{host}")).map_err(|(e, _)| e)
}

async fn handle_message(msg: RawJetstreamMessage<'static>, shared: &Arc<SharedState>) {
    let RawJetstreamMessage::Commit { did, commit, .. } = msg else {
        return;
    };
    if !matches!(commit.operation, CommitOperation::Create) {
        return;
    }
    let collection = commit.collection.to_string();
    let Some(record) = commit.record else { return };
    let rkey = commit.rkey.to_string();
    let did_str = did.to_string();
    match collection.as_str() {
        COLLECTION => handle_chat_message(did_str, rkey, record, shared).await,
        FOLLOW_COLLECTION => handle_follow(did_str, record, shared).await,
        TELEPORT_COLLECTION => handle_teleport(did_str, record, shared).await,
        _ => {}
    }
}

fn raw_str<'a>(data: &'a RawData<'_>, key: &str) -> Option<&'a str> {
    data.as_object()?.get(key)?.as_str()
}

fn raw_to_usize(v: &RawData<'_>) -> Option<usize> {
    match v {
        RawData::UnsignedInt(n) => Some(*n as usize),
        RawData::SignedInt(n) if *n >= 0 => Some(*n as usize),
        _ => None,
    }
}

fn parse_raw_facets(arr: &[RawData<'_>]) -> Vec<Facet> {
    arr.iter()
        .filter_map(|raw| {
            let obj = raw.as_object()?;
            let index_obj = obj.get("index")?.as_object()?;
            let byte_start = raw_to_usize(index_obj.get("byteStart")?)?;
            let byte_end = raw_to_usize(index_obj.get("byteEnd")?)?;
            let features = obj
                .get("features")?
                .as_array()?
                .iter()
                .filter_map(|f| {
                    let fobj = f.as_object()?;
                    let type_ = fobj.get("$type")?.as_str()?.to_string();
                    let did = fobj
                        .get("did")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let uri = fobj
                        .get("uri")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    Some(FacetFeature { type_, did, uri })
                })
                .collect();
            Some(Facet {
                index: ByteSlice {
                    byte_start,
                    byte_end,
                },
                features,
            })
        })
        .collect()
}

async fn handle_chat_message(did: String, rkey: String, record: RawData<'static>, shared: &Arc<SharedState>) {
    let streamer = match raw_str(&record, "streamer") {
        Some(s) => s.to_string(),
        None => return,
    };
    if !shared.streamers.contains(&streamer) {
        return;
    }

    let mut text = match raw_str(&record, "text") {
        Some(t) => t.to_string(),
        None => return,
    };

    let facets: Vec<Facet> = record
        .as_object()
        .and_then(|obj| obj.get("facets"))
        .and_then(|v| v.as_array())
        .map(|arr| parse_raw_facets(arr))
        .unwrap_or_default();

    if !facets.is_empty() {
        let mut sorted = facets;
        sorted.sort_by(|a, b| b.index.byte_start.cmp(&a.index.byte_start));
        for facet in sorted {
            for feature in &facet.features {
                if feature.type_ == "app.bsky.richtext.facet#mention" {
                    if let Some(mention_did) = &feature.did {
                        if let Some(handle) = shared.user_cache.resolve_handle(mention_did).await {
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
            }
        }
    }

    let display_name = shared.user_cache.resolve_handle(&did).await;
    info!(did = %did, streamer = %streamer, text = %text, "received chat message");
    let xrpc = shared.xrpc.clone();
    let user_cache = shared.user_cache.clone();
    let streamer_for_reply = streamer.clone();

    let (reply_tx, mut reply_rx) = mpsc::channel::<OutgoingMessage>(8);
    tokio::spawn(async move {
        while let Some(msg) = reply_rx.recv().await {
            let facets = build_outgoing_facets(&msg.text, &user_cache).await;
            if let Err(e) = xrpc
                .send_chat_message(&msg.text, &streamer_for_reply, facets)
                .await
            {
                error!("failed to send reply: {e}");
            }
        }
    });

    let xrpc_for_delete = shared.xrpc.clone();
    let event = tweezer::Event::Message(
        IncomingMessage::new(
            "streamplace",
            User {
                name: did.clone(),
                id: did,
                display_name,
                color: None,
                labels: Vec::new(),
                badges: Vec::new(),
            },
            text,
            streamer,
            reply_tx,
            shared.emote_fn.clone(),
        )
        .max_reply_graphemes(300)
        .message_id(&rkey)
        .on_delete(move || {
            let xrpc = xrpc_for_delete.clone();
            let rkey = rkey.clone();
            async move {
                xrpc.delete_chat_message(&rkey)
                    .await
                    .map_err(|e| tweezer::TweezerError::Handler(e))
            }
        }),
    );
    shared.bot_tx.send(event).await.ok();
}

async fn handle_follow(did: String, record: RawData<'static>, shared: &Arc<SharedState>) {
    let subject = match raw_str(&record, "subject") {
        Some(s) => s.to_string(),
        None => return,
    };
    if !shared.streamers.contains(&subject) {
        return;
    }

    let display_name = shared.user_cache.resolve_handle(&did).await;
    info!(follower = %did, streamer = %subject, "follow event");

    let xrpc = shared.xrpc.clone();
    let user_cache = shared.user_cache.clone();
    let subject_for_reply = subject.clone();
    let (reply_tx, mut reply_rx) = mpsc::channel::<OutgoingMessage>(8);
    tokio::spawn(async move {
        while let Some(msg) = reply_rx.recv().await {
            let facets = build_outgoing_facets(&msg.text, &user_cache).await;
            if let Err(e) = xrpc
                .send_chat_message(&msg.text, &subject_for_reply, facets)
                .await
            {
                error!("failed to send follow reply: {e}");
            }
        }
    });

    let event = tweezer::Event::Trigger(TriggerEvent::new(
        "streamplace",
        subject,
        TriggerKind::Follow {
            user: User {
                name: did.clone(),
                id: did,
                display_name,
                color: None,
                labels: Vec::new(),
                badges: Vec::new(),
            },
        },
        reply_tx,
        Arc::new(|name| format!(":{}:", name)),
    ));
    shared.bot_tx.send(event).await.ok();
}

async fn handle_teleport(did: String, record: RawData<'static>, shared: &Arc<SharedState>) {
    let streamer = match raw_str(&record, "streamer") {
        Some(s) => s.to_string(),
        None => return,
    };
    if !shared.streamers.contains(&streamer) {
        return;
    }

    info!(raider = %did, streamer = %streamer, "teleport/raid event");

    let xrpc = shared.xrpc.clone();
    let user_cache = shared.user_cache.clone();
    let streamer_for_reply = streamer.clone();
    let (reply_tx, mut reply_rx) = mpsc::channel::<OutgoingMessage>(8);
    tokio::spawn(async move {
        while let Some(msg) = reply_rx.recv().await {
            let facets = build_outgoing_facets(&msg.text, &user_cache).await;
            if let Err(e) = xrpc
                .send_chat_message(&msg.text, &streamer_for_reply, facets)
                .await
            {
                error!("failed to send raid reply: {e}");
            }
        }
    });

    let event = tweezer::Event::Trigger(TriggerEvent::new(
        "streamplace",
        streamer,
        TriggerKind::Raid {
            from_channel: did.clone(),
            viewer_count: None,
        },
        reply_tx,
        Arc::new(|name| format!(":{}:", name)),
    ));
    shared.bot_tx.send(event).await.ok();
}

async fn build_outgoing_facets(text: &str, cache: &UserCache) -> Option<Vec<OutgoingFacet>> {
    let scanned = scan_text_facets(text);
    if scanned.is_empty() {
        return None;
    }

    let mut facets = Vec::with_capacity(scanned.len());
    for s in scanned {
        match s {
            ScannedFacet::Mention {
                byte_start,
                byte_end,
                handle,
            } => {
                if let Some(did) = cache.resolve_did(&handle).await {
                    facets.push(OutgoingFacet::mention(byte_start, byte_end, did));
                }
            }
            ScannedFacet::Link {
                byte_start,
                byte_end,
            } => {
                let url = text[byte_start..byte_end].to_string();
                facets.push(OutgoingFacet::link(byte_start, byte_end, url));
            }
            ScannedFacet::BareLink {
                byte_start,
                byte_end,
            } => {
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
    let mut result = String::with_capacity(s.len() - (end - start) + replacement.len());
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
            ScannedFacet::Mention {
                byte_start,
                byte_end,
                handle,
            } => {
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
            ScannedFacet::Link {
                byte_start,
                byte_end,
            } => {
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
            ScannedFacet::BareLink {
                byte_start,
                byte_end,
            } => {
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
            ScannedFacet::BareLink {
                byte_start,
                byte_end,
            } => {
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
            ScannedFacet::Mention {
                byte_start,
                byte_end,
                handle,
            } => {
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
            ScannedFacet::Link {
                byte_start,
                byte_end,
            } => {
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
