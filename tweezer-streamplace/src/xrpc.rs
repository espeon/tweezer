use std::time::Duration;

use mini_moka::sync::Cache;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

#[derive(Debug)]
pub struct XrpcClient {
    client: reqwest::Client,
    service: String,
    access_jwt: String,
    refresh_jwt: String,
    did: String,
}

#[cfg(test)]
impl XrpcClient {
    pub fn test_new(service: String, access_jwt: String, refresh_jwt: String, did: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            service,
            access_jwt,
            refresh_jwt,
            did,
        }
    }
}

#[derive(Serialize)]
struct CreateSessionInput {
    identifier: String,
    password: String,
}

#[derive(Deserialize)]
struct CreateSessionOutput {
    did: String,
    #[serde(rename = "accessJwt")]
    access_jwt: String,
    #[serde(rename = "refreshJwt")]
    refresh_jwt: String,
}

#[derive(Deserialize)]
struct RefreshSessionOutput {
    #[serde(rename = "accessJwt")]
    access_jwt: String,
    #[serde(rename = "refreshJwt")]
    refresh_jwt: String,
}

#[derive(Serialize)]
struct CreateRecordInput {
    repo: String,
    collection: String,
    record: ChatMessageRecord,
}

#[derive(Serialize)]
struct DeleteRecordInput {
    repo: String,
    collection: String,
    rkey: String,
}

#[derive(Serialize)]
struct ChatMessageRecord {
    #[serde(rename = "$type")]
    type_: String,
    text: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    streamer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    facets: Option<Vec<OutgoingFacet>>,
}

#[derive(Serialize)]
pub struct OutgoingFacet {
    #[serde(rename = "$type")]
    type_: String,
    index: OutgoingByteSlice,
    features: Vec<OutgoingFacetFeature>,
}

#[derive(Serialize)]
struct OutgoingByteSlice {
    #[serde(rename = "byteStart")]
    byte_start: usize,
    #[serde(rename = "byteEnd")]
    byte_end: usize,
}

#[derive(Serialize)]
pub struct OutgoingFacetFeature {
    #[serde(rename = "$type")]
    pub type_: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

impl OutgoingFacet {
    pub fn mention(byte_start: usize, byte_end: usize, did: String) -> Self {
        Self {
            type_: "app.bsky.richtext.facet".to_string(),
            index: OutgoingByteSlice { byte_start, byte_end },
            features: vec![OutgoingFacetFeature {
                type_: "app.bsky.richtext.facet#mention".to_string(),
                did: Some(did),
                uri: None,
            }],
        }
    }

    pub fn link(byte_start: usize, byte_end: usize, url: String) -> Self {
        Self {
            type_: "app.bsky.richtext.facet".to_string(),
            index: OutgoingByteSlice { byte_start, byte_end },
            features: vec![OutgoingFacetFeature {
                type_: "app.bsky.richtext.facet#link".to_string(),
                did: None,
                uri: Some(url),
            }],
        }
    }

    pub fn emote(byte_start: usize, byte_end: usize, emote_uri: String) -> Self {
        Self {
            type_: "place.stream.richtext.facet".to_string(),
            index: OutgoingByteSlice { byte_start, byte_end },
            features: vec![OutgoingFacetFeature {
                type_: "place.stream.richtext.facet#emote".to_string(),
                did: None,
                uri: Some(emote_uri),
            }],
        }
    }
}

#[derive(Deserialize)]
struct DidDocument {
    service: Option<Vec<DidService>>,
    #[serde(rename = "alsoKnownAs")]
    also_known_as: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct DidService {
    id: String,
    #[serde(rename = "serviceEndpoint")]
    service_endpoint: String,
}

pub async fn resolve_pds(identifier: &str) -> Result<String, String> {
    info!(identifier, "resolving PDS");
    let client = reqwest::Client::new();

    let did = if identifier.starts_with("did:") {
        identifier.to_string()
    } else {
        resolve_handle(&client, identifier).await?
    };

    debug!(did = %did, "fetching DID document");
    let did_doc = fetch_did_document(&client, &did).await?;
    let pds = extract_pds(&did_doc)?;
    info!(did = %did, pds = %pds, "resolved PDS");
    Ok(pds)
}

async fn resolve_handle(client: &reqwest::Client, handle: &str) -> Result<String, String> {
    let url = format!(
        "https://{}/.well-known/atproto-did",
        handle.trim_start_matches('@')
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("handle resolution request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("handle resolution failed ({status}): {body}"));
    }

    let text = resp
        .text()
        .await
        .map_err(|e| format!("handle resolution read failed: {e}"))?;

    let did = text.trim();
    if did.starts_with("did:") {
        Ok(did.to_string())
    } else {
        Err(format!("invalid DID from handle resolution: {did}"))
    }
}

#[derive(Clone)]
pub struct UserCache {
    handle_cache: Cache<String, String>,
    did_cache: Cache<String, String>,
    client: reqwest::Client,
}

impl UserCache {
    pub fn new() -> Self {
        Self {
            handle_cache: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(10 * 60))
                .time_to_idle(Duration::from_secs(5 * 60))
                .build(),
            did_cache: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(10 * 60))
                .time_to_idle(Duration::from_secs(5 * 60))
                .build(),
            client: reqwest::Client::new(),
        }
    }

    pub async fn resolve_handle(&self, did: &str) -> Option<String> {
        let did_key = did.to_string();
        if let Some(handle) = self.handle_cache.get(&did_key) {
            return Some(handle);
        }

        match fetch_did_document(&self.client, &did_key).await {
            Ok(doc) => {
                let handle = doc
                    .also_known_as
                    .as_ref()
                    .and_then(|akas| {
                        akas.iter()
                            .find(|a| a.starts_with("at://"))
                            .map(|a| a.trim_start_matches("at://").to_string())
                            .or_else(|| akas.first().cloned())
                    })
                    .or_else(|| {
                        did.strip_prefix("did:web:").map(|domain| domain.to_string())
                    });

                if let Some(ref h) = handle {
                    self.handle_cache.insert(did.to_string(), h.clone());
                    self.did_cache.insert(h.clone(), did.to_string());
                    debug!(did = %did, handle = %h, "resolved handle");
                }
                handle
            }
            Err(e) => {
                warn!(did = %did, error = %e, "failed to resolve handle");
                None
            }
        }
    }

    pub async fn resolve_did(&self, handle: &str) -> Option<String> {
        let handle_key = handle.to_string();
        if let Some(did) = self.did_cache.get(&handle_key) {
            return Some(did);
        }

        match resolve_handle(&self.client, &handle_key).await {
            Ok(did) => {
                self.did_cache.insert(handle.to_string(), did.clone());
                self.handle_cache.insert(did.clone(), handle.to_string());
                debug!(handle = %handle, did = %did, "resolved DID");
                Some(did)
            }
            Err(e) => {
                warn!(handle = %handle, error = %e, "failed to resolve DID");
                None
            }
        }
    }
}

async fn fetch_did_document(client: &reqwest::Client, did: &str) -> Result<DidDocument, String> {
    let url = if did.strip_prefix("did:plc:").is_some() {
        format!("https://plc.directory/{did}")
    } else if let Some(domain) = did.strip_prefix("did:web:") {
        format!("https://{domain}/.well-known/did.json")
    } else {
        return Err(format!("unsupported DID method: {did}"));
    };

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("DID document fetch failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("DID document fetch failed ({status}): {body}"));
    }

    resp.json()
        .await
        .map_err(|e| format!("DID document decode failed: {e}"))
}

fn extract_pds(doc: &DidDocument) -> Result<String, String> {
    let services = doc.service.as_ref();
    let endpoint = services
        .and_then(|svcs| {
            svcs.iter().find(|s| {
                s.id.contains("atproto_pds")
                    || s.id.contains("AtprotoPersonalDataServer")
                    || s.id == "#atproto_pds"
            })
        })
        .map(|s| s.service_endpoint.trim_end_matches('/').to_string());

    endpoint.ok_or_else(|| "no atproto_pds service found in DID document".to_string())
}

impl XrpcClient {
    pub async fn create(
        service: String,
        identifier: String,
        password: String,
    ) -> Result<Self, String> {
        let client = reqwest::Client::new();
        let url = format!("{}/xrpc/com.atproto.server.createSession", service);

        let resp = client
            .post(&url)
            .json(&CreateSessionInput {
                identifier,
                password,
            })
            .send()
            .await
            .map_err(|e| format!("session request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("session creation failed ({status}): {body}"));
        }

        let session: CreateSessionOutput = resp
            .json()
            .await
            .map_err(|e| format!("session decode failed: {e}"))?;

        info!(did = %session.did, "authenticated");

        Ok(Self {
            client,
            service,
            access_jwt: session.access_jwt,
            refresh_jwt: session.refresh_jwt,
            did: session.did,
        })
    }

    pub fn did(&self) -> &str {
        &self.did
    }

    /// Exchange the refresh JWT for a new access+refresh JWT pair.
    pub async fn refresh(&mut self) -> Result<(), String> {
        let url = format!("{}/xrpc/com.atproto.server.refreshSession", self.service);

        let resp = self.client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.refresh_jwt))
            .send()
            .await
            .map_err(|e| format!("refresh request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("session refresh failed ({status}): {body}"));
        }

        let session: RefreshSessionOutput = resp
            .json()
            .await
            .map_err(|e| format!("refresh decode failed: {e}"))?;

        self.access_jwt = session.access_jwt;
        self.refresh_jwt = session.refresh_jwt;
        info!(did = %self.did, "session refreshed");
        Ok(())
    }

    pub async fn send_chat_message(
        &self,
        text: &str,
        streamer_did: &str,
        facets: Option<Vec<OutgoingFacet>>,
    ) -> Result<(), String> {
        let url = format!("{}/xrpc/com.atproto.repo.createRecord", self.service);

        let now = chrono_like_iso_now();

        let resp = self.client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_jwt))
            .json(&CreateRecordInput {
                repo: self.did.clone(),
                collection: "place.stream.chat.message".to_string(),
                record: ChatMessageRecord {
                    type_: "place.stream.chat.message".to_string(),
                    text: text.to_string(),
                    created_at: now,
                    streamer: streamer_did.to_string(),
                    facets,
                },
            })
            .send()
            .await
            .map_err(|e| format!("createRecord request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(%status, %body, "failed to send message");
            return Err(format!("createRecord failed ({status}): {body}"));
        }

        Ok(())
    }

    pub async fn delete_chat_message(&self, rkey: &str) -> Result<(), String> {
        let url = format!("{}/xrpc/com.atproto.repo.deleteRecord", self.service);

        let resp = self.client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_jwt))
            .json(&DeleteRecordInput {
                repo: self.did.clone(),
                collection: "place.stream.chat.message".to_string(),
                rkey: rkey.to_string(),
            })
            .send()
            .await
            .map_err(|e| format!("deleteRecord request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(%status, %body, "failed to delete message");
            return Err(format!("deleteRecord failed ({status}): {body}"));
        }

        Ok(())
    }

    pub async fn create_gate(&self, streamer: &str, message_uri: &str) -> Result<(), String> {
        let url = format!("{}/xrpc/place.stream.moderation.createGate", self.service);
        let resp = self.client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_jwt))
            .json(&serde_json::json!({
                "streamer": streamer,
                "hiddenMessage": message_uri,
                "createdAt": chrono_like_iso_now(),
            }))
            .send()
            .await
            .map_err(|e| format!("createGate request failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("createGate failed ({status}): {body}"));
        }
        Ok(())
    }

    pub async fn create_block(&self, streamer: &str, subject: &str) -> Result<(), String> {
        let url = format!("{}/xrpc/place.stream.moderation.createBlock", self.service);
        let resp = self.client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_jwt))
            .json(&serde_json::json!({
                "streamer": streamer,
                "subject": subject,
                "createdAt": chrono_like_iso_now(),
            }))
            .send()
            .await
            .map_err(|e| format!("createBlock request failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("createBlock failed ({status}): {body}"));
        }
        Ok(())
    }

    pub async fn create_pin(
        &self,
        streamer: &str,
        message_uri: &str,
        expires_at: Option<&str>,
    ) -> Result<(), String> {
        let url = format!("{}/xrpc/place.stream.moderation.createPin", self.service);
        let mut body = serde_json::json!({
            "streamer": streamer,
            "pinnedMessage": message_uri,
            "createdAt": chrono_like_iso_now(),
        });
        if let Some(exp) = expires_at {
            body["expiresAt"] = exp.into();
        }
        let resp = self.client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_jwt))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("createPin request failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("createPin failed ({status}): {body}"));
        }
        Ok(())
    }

    pub async fn update_chat_profile(
        &self,
        color: Option<&serde_json::Value>,
        labels: &[String],
    ) -> Result<(), String> {
        let url = format!("{}/xrpc/com.atproto.repo.putRecord", self.service);
        let mut record = serde_json::json!({
            "$type": "place.stream.chat.profile",
        });
        if let Some(c) = color {
            record["color"] = c.clone();
        }
        if !labels.is_empty() {
            record["selfLabels"] = labels.iter().map(|l| serde_json::json!(l)).collect();
        }
        let resp = self.client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_jwt))
            .json(&serde_json::json!({
                "repo": self.did,
                "collection": "place.stream.chat.profile",
                "record": record,
            }))
            .send()
            .await
            .map_err(|e| format!("putRecord chat profile failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("putRecord chat profile failed ({status}): {body}"));
        }
        Ok(())
    }
}

fn chrono_like_iso_now() -> String {
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let millis = duration.subsec_millis();
    let days_since_epoch = secs / 86400;
    let time_of_day_secs = secs % 86400;

    let (year, month, day) = days_to_ymd(days_since_epoch as i64);
    let hours = time_of_day_secs / 3600;
    let minutes = (time_of_day_secs % 3600) / 60;
    let seconds = time_of_day_secs % 60;

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}.{millis:03}Z")
}

fn days_to_ymd(mut days: i64) -> (i64, i64, i64) {
    let mut year = 1970;
    loop {
        let dy = if is_leap(year) { 366 } else { 365 };
        if days < dy {
            break;
        }
        days -= dy;
        year += 1;
    }
    let leap = is_leap(year);
    let mdays = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0;
    for &m in &mdays {
        if days < m {
            break;
        }
        days -= m;
        month += 1;
    }
    (year, month + 1, days + 1)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_leap_basic() {
        assert!(is_leap(2000));
        assert!(is_leap(2024));
        assert!(!is_leap(1900));
        assert!(!is_leap(2023));
    }

    #[test]
    fn days_to_ymd_epoch() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn days_to_ymd_known_dates() {
        assert_eq!(days_to_ymd(1), (1970, 1, 2));
        assert_eq!(days_to_ymd(31), (1970, 2, 1));
        assert_eq!(days_to_ymd(365), (1971, 1, 1));
        assert_eq!(days_to_ymd(366), (1971, 1, 2));
    }

    #[test]
    fn days_to_ymd_leap_year() {
        // 1972 is a leap year. day 0 = jan 1 1970
        // days from 1970-01-01 to 1972-02-29:
        // 1970: 365, 1971: 365 = 730 days to 1972-01-01
        // +31 (jan) +28 (feb 1-28) + 1 more = feb 29
        let feb_29 = 730 + 31 + 28;
        assert_eq!(days_to_ymd(feb_29), (1972, 2, 29));
        let mar_1 = feb_29 + 1;
        assert_eq!(days_to_ymd(mar_1), (1972, 3, 1));
    }

    #[test]
    fn chrono_like_iso_now_format() {
        let ts = chrono_like_iso_now();
        // should match YYYY-MM-DDTHH:MM:SS.sssZ
        let re = regex_lite::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$").unwrap();
        assert!(
            re.is_match(&ts),
            "timestamp '{ts}' doesn't match expected format"
        );
    }

    #[test]
    fn chat_message_record_serializes_correctly() {
        let record = ChatMessageRecord {
            type_: "place.stream.chat.message".to_string(),
            text: "hello world".to_string(),
            created_at: "2024-01-01T00:00:00.000Z".to_string(),
            streamer: "did:plc:test".to_string(),
            facets: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains(r#""$type":"place.stream.chat.message""#));
        assert!(json.contains(r#""text":"hello world""#));
        assert!(json.contains(r#""createdAt":"2024-01-01T00:00:00.000Z""#));
        assert!(json.contains(r#""streamer":"did:plc:test""#));
        assert!(!json.contains("facets"));
    }

    #[test]
    fn chat_message_record_serializes_with_facets() {
        let record = ChatMessageRecord {
            type_: "place.stream.chat.message".to_string(),
            text: "hi @alice.bsky.social".to_string(),
            created_at: "2024-01-01T00:00:00.000Z".to_string(),
            streamer: "did:plc:test".to_string(),
            facets: Some(vec![OutgoingFacet::mention(
                3,
                22,
                "did:plc:abc".to_string(),
            )]),
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains(r#""facets""#));
        assert!(json.contains(r#""byteStart":3"#));
        assert!(json.contains(r#""byteEnd":22"#));
        assert!(json.contains(r#""did":"did:plc:abc""#));
        assert!(json.contains(r#"app.bsky.richtext.facet#mention"#));
    }

    #[test]
    fn outgoing_facet_link_serializes() {
        let facet = OutgoingFacet::link(0, 19, "https://example.com".to_string());
        let json = serde_json::to_string(&facet).unwrap();
        assert!(json.contains(r#""uri":"https://example.com""#));
        assert!(json.contains(r#"app.bsky.richtext.facet#link"#));
        assert!(!json.contains("did"));
    }

    #[test]
    fn create_record_input_serializes_correctly() {
        let input = CreateRecordInput {
            repo: "did:plc:bot".to_string(),
            collection: "place.stream.chat.message".to_string(),
            record: ChatMessageRecord {
                type_: "place.stream.chat.message".to_string(),
                text: "hi".to_string(),
                created_at: "2024-01-01T00:00:00.000Z".to_string(),
                streamer: "did:plc:streamer".to_string(),
                facets: None,
            },
        };
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains(r#""repo":"did:plc:bot""#));
        assert!(json.contains(r#""collection":"place.stream.chat.message""#));
        assert!(json.contains(r#""record""#));
    }

    #[test]
    fn create_session_input_serializes() {
        let input = CreateSessionInput {
            identifier: "user@example.com".to_string(),
            password: "secret".to_string(),
        };
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains(r#""identifier":"user@example.com""#));
        assert!(json.contains(r#""password":"secret""#));
    }

    #[test]
    fn extract_pds_from_did_doc() {
        let doc = DidDocument {
            service: Some(vec![DidService {
                id: "#atproto_pds".into(),
                service_endpoint: "https://pds.example.com/".into(),
            }]),
            also_known_as: None,
        };
        assert_eq!(extract_pds(&doc).unwrap(), "https://pds.example.com");
    }

    #[test]
    fn extract_pds_no_service() {
        let doc = DidDocument {
            service: None,
            also_known_as: None,
        };
        assert!(extract_pds(&doc).is_err());
    }

    #[test]
    fn extract_pds_no_matching_service() {
        let doc = DidDocument {
            service: Some(vec![DidService {
                id: "#some_other".into(),
                service_endpoint: "https://other.example.com".into(),
            }]),
            also_known_as: None,
        };
        assert!(extract_pds(&doc).is_err());
    }

    #[test]
    fn extract_pds_strips_trailing_slash() {
        let doc = DidDocument {
            service: Some(vec![DidService {
                id: "#atproto_pds".into(),
                service_endpoint: "https://pds.example.com///".into(),
            }]),
            also_known_as: None,
        };
        let result = extract_pds(&doc).unwrap();
        assert_eq!(result, "https://pds.example.com");
    }
}
