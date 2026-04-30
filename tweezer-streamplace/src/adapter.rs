use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, RwLock},
};

use jacquard_common::deps::smol_str::SmolStr;

use async_trait::async_trait;
use jacquard_common::{
    CowStr, RawData,
    deps::fluent_uri::{ParseError, Uri},
    jetstream::{CommitOperation, RawJetstreamMessage, RawJetstreamParams},
    xrpc::{SubscriptionClient, TungsteniteSubscriptionClient},
};
use n0_future::StreamExt;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tweezer::{
    Adapter, BotTx, IncomingMessage, ModerationAction, OutgoingMessage, ReplyRef, TriggerEvent,
    TriggerKind, TweezerError, User,
};

use crate::xrpc::{OutgoingFacet, UserCache, XrpcClient, resolve_pds};

const COLLECTION: &str = "place.stream.chat.message";
const FOLLOW_COLLECTION: &str = "app.bsky.graph.follow";
const TELEPORT_COLLECTION: &str = "place.stream.live.teleport";
const CHAT_PROFILE_COLLECTION: &str = "place.stream.chat.profile";
const GATE_COLLECTION: &str = "place.stream.chat.gate";
const PIN_COLLECTION: &str = "place.stream.chat.pinnedRecord";
const BLOCK_COLLECTION: &str = "app.bsky.graph.block";
const MOD_PERMISSION_COLLECTION: &str = "place.stream.moderation.permission";
const BADGE_DEF_COLLECTION: &str = "place.stream.badge.def";
const BADGE_ISSUANCE_COLLECTION: &str = "place.stream.badge.issuance";
const EMOTE_PACK_COLLECTION: &str = "place.stream.emote.pack";
const EMOTE_ITEM_COLLECTION: &str = "place.stream.emote.item";
const EMOTE_DELEGATION_COLLECTION: &str = "place.stream.emote.packDelegation";

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

#[derive(Clone, Default)]
struct ChatProfile {
    color: Option<tweezer::ChatColor>,
    labels: Vec<String>,
    badges: Vec<tweezer::BadgeInfo>,
}

/// Per-streamer state indexed from the firehose.
#[derive(Clone, Default)]
struct StreamerState {
    blocked_dids: HashSet<String>,
    mod_dids: HashSet<String>,
}

#[derive(Clone)]
struct EmoteItem {
    name: String,
    pack_uri: String,
    image_cid: String,
    alt: String,
}

struct SharedState {
    bot_tx: BotTx,
    streamers: HashSet<String>,
    xrpc: Arc<XrpcClient>,
    user_cache: UserCache,
    emote_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
    chat_profiles: RwLock<HashMap<String, ChatProfile>>,
    streamer_states: RwLock<HashMap<String, StreamerState>>,
    emote_items: RwLock<HashMap<String, EmoteItem>>,
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
            chat_profiles: RwLock::new(HashMap::new()),
            streamer_states: RwLock::new(HashMap::new()),
            emote_items: RwLock::new(HashMap::new()),
        });

        info!(
            jetstream = %self.jetstream_url,
            streamers = ?self.streamers,
            did = %shared.xrpc.did(),
            "connected to streamplace"
        );

        // Self-label as bot in chat profile.
        let xrpc_for_profile = shared.xrpc.clone();
        tokio::spawn(async move {
            if let Err(e) = xrpc_for_profile
                .update_chat_profile(None, &["bot".to_string()])
                .await
            {
                warn!(error = %e, "failed to set chat profile");
            }
        });

        tokio::spawn(async move {
            let client = TungsteniteSubscriptionClient::from_base_uri(base_uri);
            let params = RawJetstreamParams::new()
                .wanted_collections(vec![
                    CowStr::from(COLLECTION),
                    CowStr::from(FOLLOW_COLLECTION),
                    CowStr::from(TELEPORT_COLLECTION),
                    CowStr::from(CHAT_PROFILE_COLLECTION),
                    CowStr::from(GATE_COLLECTION),
                    CowStr::from(PIN_COLLECTION),
                    CowStr::from(BLOCK_COLLECTION),
                    CowStr::from(MOD_PERMISSION_COLLECTION),
                    CowStr::from(BADGE_DEF_COLLECTION),
                    CowStr::from(BADGE_ISSUANCE_COLLECTION),
                    CowStr::from(EMOTE_PACK_COLLECTION),
                    CowStr::from(EMOTE_ITEM_COLLECTION),
                    CowStr::from(EMOTE_DELEGATION_COLLECTION),
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
    let collection = commit.collection.to_string();
    let Some(record) = commit.record else { return };
    let rkey = commit.rkey.to_string();
    let did_str = did.to_string();

    match commit.operation {
        CommitOperation::Create => match collection.as_str() {
            COLLECTION => handle_chat_message(did_str, rkey, record, shared).await,
            FOLLOW_COLLECTION => handle_follow(did_str, record, shared).await,
            TELEPORT_COLLECTION => handle_teleport(did_str, record, shared).await,
            CHAT_PROFILE_COLLECTION => handle_chat_profile(did_str, record, shared).await,
            GATE_COLLECTION => handle_gate(did_str, record, shared).await,
            PIN_COLLECTION => handle_pin(did_str, record, shared).await,
            BLOCK_COLLECTION => handle_block(did_str, rkey, record, shared).await,
            MOD_PERMISSION_COLLECTION => handle_mod_permission(did_str, record, shared).await,
            BADGE_DEF_COLLECTION => handle_badge_def(rkey, record, shared).await,
            BADGE_ISSUANCE_COLLECTION => handle_badge_issuance(did_str, record, shared).await,
            EMOTE_PACK_COLLECTION => handle_emote_pack(did_str, rkey, record, shared).await,
            EMOTE_ITEM_COLLECTION => handle_emote_item(did_str, rkey, record, shared).await,
            EMOTE_DELEGATION_COLLECTION => handle_emote_delegation(did_str, record, shared).await,
            _ => {}
        },
        CommitOperation::Delete => match collection.as_str() {
            BLOCK_COLLECTION => handle_block_delete(did_str, rkey, shared).await,
            MOD_PERMISSION_COLLECTION => handle_mod_permission_delete(did_str, rkey, shared).await,
            _ => {}
        },
        CommitOperation::Update => {}
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

fn parse_raw_color(obj: &BTreeMap<SmolStr, RawData<'_>>) -> Option<tweezer::ChatColor> {
    let red = raw_to_usize(obj.get("red")?)? as u8;
    let green = raw_to_usize(obj.get("green")?)? as u8;
    let blue = raw_to_usize(obj.get("blue")?)? as u8;
    Some(tweezer::ChatColor { red, green, blue })
}

fn parse_chat_profile(record: &RawData<'_>) -> Option<ChatProfile> {
    let obj = record.as_object()?;
    let color = obj.get("color").and_then(|c| c.as_object()).and_then(parse_raw_color);
    let labels = obj
        .get("selfLabels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    Some(ChatProfile {
        color,
        labels,
        badges: Vec::new(),
    })
}

fn parse_reply_ref(record: &RawData<'_>) -> Option<ReplyRef> {
    let obj = record.as_object()?;
    let reply = obj.get("reply")?.as_object()?;
    let root = reply.get("root")?.as_object()?;
    let parent = reply.get("parent")?.as_object()?;
    let root_uri = root.get("uri")?.as_str()?.to_string();
    let root_cid = root.get("cid")?.as_str()?.to_string();
    let parent_uri = parent.get("uri")?.as_str()?.to_string();
    let parent_cid = parent.get("cid")?.as_str()?.to_string();
    Some(ReplyRef {
        root_uri,
        root_cid,
        parent_uri,
        parent_cid,
    })
}

fn hydrate_user(did: &str, display_name: Option<String>, shared: &SharedState) -> User {
    let (color, labels, badges) = shared
        .chat_profiles
        .read()
        .ok()
        .and_then(|profiles| profiles.get(did).cloned())
        .map(|p| (p.color, p.labels, p.badges))
        .unwrap_or_default();
    User {
        name: did.to_string(),
        id: did.to_string(),
        display_name,
        color,
        labels,
        badges,
    }
}

fn is_blocked(shared: &SharedState, streamer: &str, did: &str) -> bool {
    shared
        .streamer_states
        .read()
        .ok()
        .and_then(|states| states.get(streamer).cloned())
        .map(|s| s.blocked_dids.contains(did))
        .unwrap_or(false)
}

fn is_moderator(shared: &SharedState, streamer: &str, did: &str) -> bool {
    shared
        .streamer_states
        .read()
        .ok()
        .and_then(|states| states.get(streamer).cloned())
        .map(|s| s.mod_dids.contains(did))
        .unwrap_or(false)
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

    if is_blocked(shared, &streamer, &did) {
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
    let reply = parse_reply_ref(&record);
    let is_streamer = did == streamer;
    let is_moderator = is_moderator(shared, &streamer, &did);

    info!(did = %did, streamer = %streamer, text = %text, "received chat message");
    let xrpc = shared.xrpc.clone();
    let shared_for_facets = Arc::clone(shared);
    let streamer_for_reply = streamer.clone();

    let (reply_tx, mut reply_rx) = mpsc::channel::<OutgoingMessage>(8);
    tokio::spawn(async move {
        while let Some(msg) = reply_rx.recv().await {
            let facets = build_outgoing_facets(&msg.text, &shared_for_facets).await;
            if let Err(e) = xrpc
                .send_chat_message(&msg.text, &streamer_for_reply, facets)
                .await
            {
                error!("failed to send reply: {e}");
            }
        }
    });

    let xrpc_for_delete = shared.xrpc.clone();
    let xrpc_for_moderation = shared.xrpc.clone();
    let streamer_for_moderation = streamer.clone();
    let message_uri = format!("at://{did}/place.stream.chat.message/{rkey}");
    let event = tweezer::Event::Message(
        IncomingMessage::new(
            "streamplace",
            hydrate_user(&did, display_name, shared),
            text,
            streamer,
            reply_tx,
            shared.emote_fn.clone(),
        )
        .max_reply_graphemes(300)
        .message_id(&rkey)
        .reply(reply)
        .is_streamer(is_streamer)
        .is_moderator(is_moderator)
        .on_delete(move || {
            let xrpc = xrpc_for_delete.clone();
            let rkey = rkey.clone();
            async move {
                xrpc.delete_chat_message(&rkey)
                    .await
                    .map_err(|e| tweezer::TweezerError::Handler(e))
            }
        })
        .on_moderate(move |action: ModerationAction| {
            let xrpc = xrpc_for_moderation.clone();
            let streamer = streamer_for_moderation.clone();
            let message_uri = message_uri.clone();
            async move {
                match action {
                    ModerationAction::HideMessage { .. } => {
                        xrpc.create_gate(&streamer, &message_uri)
                            .await
                            .map_err(|e| tweezer::TweezerError::Handler(e))
                    }
                    ModerationAction::BanUser { user_did } => {
                        xrpc.create_block(&streamer, &user_did)
                            .await
                            .map_err(|e| tweezer::TweezerError::Handler(e))
                    }
                    ModerationAction::PinMessage { message_uri, expires_at } => {
                        xrpc.create_pin(&streamer, &message_uri, expires_at.as_deref())
                            .await
                            .map_err(|e| tweezer::TweezerError::Handler(e))
                    }
                    _ => Ok(()),
                }
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
    let shared_for_facets = Arc::clone(shared);
    let subject_for_reply = subject.clone();
    let (reply_tx, mut reply_rx) = mpsc::channel::<OutgoingMessage>(8);
    tokio::spawn(async move {
        while let Some(msg) = reply_rx.recv().await {
            let facets = build_outgoing_facets(&msg.text, &shared_for_facets).await;
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
            user: hydrate_user(&did, display_name, shared),
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
    let shared_for_facets = Arc::clone(shared);
    let streamer_for_reply = streamer.clone();
    let (reply_tx, mut reply_rx) = mpsc::channel::<OutgoingMessage>(8);
    tokio::spawn(async move {
        while let Some(msg) = reply_rx.recv().await {
            let facets = build_outgoing_facets(&msg.text, &shared_for_facets).await;
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

async fn handle_chat_profile(did: String, record: RawData<'static>, shared: &Arc<SharedState>) {
    let Some(profile) = parse_chat_profile(&record) else {
        return;
    };
    if let Ok(mut profiles) = shared.chat_profiles.write() {
        profiles.insert(did, profile);
    }
}

async fn handle_gate(did: String, record: RawData<'static>, shared: &Arc<SharedState>) {
    let hidden_message = match raw_str(&record, "hiddenMessage") {
        Some(s) => s.to_string(),
        None => return,
    };
    let streamer = did;
    if !shared.streamers.contains(&streamer) {
        return;
    }
    info!(message = %hidden_message, "message hidden");
    let event = tweezer::Event::Trigger(TriggerEvent::new(
        "streamplace",
        streamer.clone(),
        TriggerKind::MessageHidden {
            message_uri: hidden_message,
            hidden_by: streamer,
        },
        mpsc::channel(1).0,
        shared.emote_fn.clone(),
    ));
    shared.bot_tx.send(event).await.ok();
}

async fn handle_pin(did: String, record: RawData<'static>, shared: &Arc<SharedState>) {
    let pinned_message = match raw_str(&record, "pinnedMessage") {
        Some(s) => s.to_string(),
        None => return,
    };
    let pinned_by = raw_str(&record, "pinnedBy").unwrap_or(&did).to_string();
    let expires_at = raw_str(&record, "expiresAt").map(|s| s.to_string());
    let streamer = did;
    if !shared.streamers.contains(&streamer) {
        return;
    }
    info!(message = %pinned_message, "message pinned");
    let event = tweezer::Event::Trigger(TriggerEvent::new(
        "streamplace",
        streamer,
        TriggerKind::MessagePinned {
            message_uri: pinned_message,
            pinned_by,
            expires_at,
        },
        mpsc::channel(1).0,
        shared.emote_fn.clone(),
    ));
    shared.bot_tx.send(event).await.ok();
}

async fn handle_block(
    did: String,
    _rkey: String,
    record: RawData<'static>,
    shared: &Arc<SharedState>,
) {
    let subject = match raw_str(&record, "subject") {
        Some(s) => s.to_string(),
        None => return,
    };
    let streamer = did;
    if !shared.streamers.contains(&streamer) {
        return;
    }
    if let Ok(mut states) = shared.streamer_states.write() {
        states
            .entry(streamer.clone())
            .or_default()
            .blocked_dids
            .insert(subject.clone());
    }
    let display_name = shared.user_cache.resolve_handle(&subject).await;
    let event = tweezer::Event::Trigger(TriggerEvent::new(
        "streamplace",
        streamer.clone(),
        TriggerKind::UserBanned {
            user: hydrate_user(&subject, display_name, shared),
            banned_by: streamer.clone(),
        },
        mpsc::channel(1).0,
        shared.emote_fn.clone(),
    ));
    shared.bot_tx.send(event).await.ok();
}

async fn handle_block_delete(did: String, _rkey: String, shared: &Arc<SharedState>) {
    let streamer = did.clone();
    if !shared.streamers.contains(&streamer) {
        return;
    }
    // We don't know the subject from the delete tombstone alone,
    // so we emit an unbanned trigger without the user DID.
    let event = tweezer::Event::Trigger(TriggerEvent::new(
        "streamplace",
        streamer.clone(),
        TriggerKind::UserUnbanned {
            user_did: String::new(),
            unbanned_by: streamer,
        },
        mpsc::channel(1).0,
        shared.emote_fn.clone(),
    ));
    shared.bot_tx.send(event).await.ok();
}

async fn handle_mod_permission(did: String, record: RawData<'static>, shared: &Arc<SharedState>) {
    let moderator = match raw_str(&record, "moderator") {
        Some(s) => s.to_string(),
        None => return,
    };
    let streamer = did;
    if !shared.streamers.contains(&streamer) {
        return;
    }
    if let Ok(mut states) = shared.streamer_states.write() {
        states
            .entry(streamer)
            .or_default()
            .mod_dids
            .insert(moderator);
    }
}

async fn handle_mod_permission_delete(
    did: String,
    _rkey: String,
    shared: &Arc<SharedState>,
) {
    let streamer = did;
    if !shared.streamers.contains(&streamer) {
        return;
    }
    // Without the full record we can't know which mod was removed,
    // so we clear all mod permissions and let them be rebuilt from firehose.
    if let Ok(mut states) = shared.streamer_states.write() {
        if let Some(state) = states.get_mut(&streamer) {
            state.mod_dids.clear();
        }
    }
}

async fn handle_badge_def(_rkey: String, _record: RawData<'static>, _shared: &Arc<SharedState>) {
    // TODO: Index badge definitions for hydration.
}

async fn handle_badge_issuance(
    _did: String,
    _record: RawData<'static>,
    _shared: &Arc<SharedState>,
) {
    // TODO: Index badge issuances for user hydration.
}

async fn handle_emote_pack(
    _did: String,
    _rkey: String,
    _record: RawData<'static>,
    _shared: &Arc<SharedState>,
) {
    // TODO: Index emote packs.
}

async fn handle_emote_item(
    did: String,
    rkey: String,
    record: RawData<'static>,
    shared: &Arc<SharedState>,
) {
    let name = match raw_str(&record, "name") {
        Some(s) => s.to_string(),
        None => return,
    };
    let pack_uri = raw_str(&record, "pack").unwrap_or("").to_string();
    let alt = raw_str(&record, "alt").unwrap_or("").to_string();

    let image_cid = record
        .as_object()
        .and_then(|obj| obj.get("image"))
        .and_then(|img| img.as_object())
        .and_then(|img| img.get("ref"))
        .and_then(|r#ref| r#ref.as_object())
        .and_then(|r#ref| r#ref.get("$link"))
        .and_then(|link| link.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();

    let uri = format!("at://{did}/place.stream.emote.item/{rkey}");
    let item = EmoteItem {
        name,
        pack_uri,
        image_cid,
        alt,
    };
    if let Ok(mut items) = shared.emote_items.write() {
        items.insert(uri, item);
    }
}

async fn handle_emote_delegation(
    _did: String,
    _record: RawData<'static>,
    _shared: &Arc<SharedState>,
) {
    // TODO: Index emote pack delegations.
}

fn scan_emote_facets(text: &str) -> Vec<(usize, usize, String)> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut facets = Vec::new();
    let mut i = 0;

    while i < len {
        if bytes[i] == b':' {
            let start = i;
            let mut j = i + 1;
            while j < len && bytes[j] != b':' && !bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < len && bytes[j] == b':' && j > start + 1 {
                let name = &text[start + 1..j];
                if name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
                    facets.push((start, j + 1, name.to_string()));
                    i = j + 1;
                    continue;
                }
            }
        }
        i += 1;
    }

    facets
}

async fn build_outgoing_facets(text: &str, shared: &SharedState) -> Option<Vec<OutgoingFacet>> {
    let scanned = scan_text_facets(text);
    let emote_scanned = scan_emote_facets(text);

    let mut facets = Vec::with_capacity(scanned.len() + emote_scanned.len());

    for s in scanned {
        match s {
            ScannedFacet::Mention {
                byte_start,
                byte_end,
                handle,
            } => {
                if let Some(did) = shared.user_cache.resolve_did(&handle).await {
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

    if !emote_scanned.is_empty() {
        let items = shared.emote_items.read().ok()?;
        for (start, end, name) in emote_scanned {
            // Find an emote item with this name.
            // TODO: validate access rights (pack delegation, openInMyChat).
            if let Some((uri, _item)) = items.iter().find(|(_, item)| item.name == name) {
                facets.push(OutgoingFacet::emote(start, end, uri.clone()));
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

    #[test]
    fn scan_emote_finds_name() {
        let facets = scan_emote_facets("hello :dan: world");
        assert_eq!(facets.len(), 1);
        assert_eq!(facets[0].0, 6);
        assert_eq!(facets[0].1, 11);
        assert_eq!(facets[0].2, "dan");
    }

    #[test]
    fn scan_emote_ignores_invalid() {
        assert!(scan_emote_facets("no emotes here").is_empty());
        assert!(scan_emote_facets("missing colon").is_empty());
        assert!(scan_emote_facets("empty ::").is_empty());
        assert!(scan_emote_facets("space :a b:").is_empty());
    }

    #[test]
    fn scan_emote_multiple() {
        let facets = scan_emote_facets(":dan::pog:");
        assert_eq!(facets.len(), 2);
        assert_eq!(facets[0].2, "dan");
        assert_eq!(facets[1].2, "pog");
    }

    #[test]
    fn hydrate_user_no_profile() {
        let shared = SharedState {
            bot_tx: mpsc::channel(1).0,
            streamers: HashSet::new(),
            xrpc: Arc::new(XrpcClient::test_new(
                "https://example.com".into(),
                "x".into(),
                "y".into(),
                "did:plc:bot".into(),
            )),
            user_cache: UserCache::new(),
            emote_fn: Arc::new(|name| format!(":{}:", name)),
            chat_profiles: RwLock::new(HashMap::new()),
            streamer_states: RwLock::new(HashMap::new()),
            emote_items: RwLock::new(HashMap::new()),
        };
        let user = hydrate_user("did:plc:test", Some("alice".into()), &shared);
        assert_eq!(user.name, "did:plc:test");
        assert_eq!(user.display_name, Some("alice".into()));
        assert!(user.color.is_none());
        assert!(user.labels.is_empty());
    }

    #[test]
    fn is_blocked_checks_state() {
        let shared = SharedState {
            bot_tx: mpsc::channel(1).0,
            streamers: HashSet::new(),
            xrpc: Arc::new(XrpcClient::test_new(
                "https://example.com".into(),
                "x".into(),
                "y".into(),
                "did:plc:bot".into(),
            )),
            user_cache: UserCache::new(),
            emote_fn: Arc::new(|name| format!(":{}:", name)),
            chat_profiles: RwLock::new(HashMap::new()),
            streamer_states: RwLock::new(HashMap::new()),
            emote_items: RwLock::new(HashMap::new()),
        };
        {
            let mut states = shared.streamer_states.write().unwrap();
            let mut state = StreamerState::default();
            state.blocked_dids.insert("did:plc:bad".into());
            states.insert("did:plc:streamer".into(), state);
        }
        assert!(is_blocked(&shared, "did:plc:streamer", "did:plc:bad"));
        assert!(!is_blocked(&shared, "did:plc:streamer", "did:plc:good"));
        assert!(!is_blocked(&shared, "did:plc:other", "did:plc:bad"));
    }

    #[test]
    fn is_moderator_checks_state() {
        let shared = SharedState {
            bot_tx: mpsc::channel(1).0,
            streamers: HashSet::new(),
            xrpc: Arc::new(XrpcClient::test_new(
                "https://example.com".into(),
                "x".into(),
                "y".into(),
                "did:plc:bot".into(),
            )),
            user_cache: UserCache::new(),
            emote_fn: Arc::new(|name| format!(":{}:", name)),
            chat_profiles: RwLock::new(HashMap::new()),
            streamer_states: RwLock::new(HashMap::new()),
            emote_items: RwLock::new(HashMap::new()),
        };
        {
            let mut states = shared.streamer_states.write().unwrap();
            let mut state = StreamerState::default();
            state.mod_dids.insert("did:plc:mod".into());
            states.insert("did:plc:streamer".into(), state);
        }
        assert!(is_moderator(&shared, "did:plc:streamer", "did:plc:mod"));
        assert!(!is_moderator(&shared, "did:plc:streamer", "did:plc:viewer"));
    }
}
