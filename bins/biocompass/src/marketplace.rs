//! Opt-in Global Default Marketplace client, persisted first-run consent (`[A1]`),
//! `--airgapped` enforcement, and sync status tracking (`DEV-07`).

use crate::state::DeploymentMode;
use biocompass_core_traits::{CompassError, CompassResult};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::RwLock;

/// Configurable production Global Default Marketplace Hub URL (`[A1]`).
pub const DEFAULT_MARKETPLACE_HUB_URL: &str = "https://hub.biocompass.alithea.bio";

const CONSENT_FILE_NAME: &str = "marketplace_consent.json";
const STALE_AFTER_SECS: i64 = 15 * 60;
const MAX_FEED_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_OFFERS_PREVIEW: usize = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketplaceConsentDecision {
    Unset,
    Accepted,
    Declined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketplaceSyncState {
    #[serde(rename = "air-gapped")]
    AirGapped,
    #[serde(rename = "disconnected")]
    Disconnected,
    #[serde(rename = "connected")]
    Connected,
    #[serde(rename = "stale")]
    Stale,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MarketplaceOfferSummary {
    pub id: String,
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_org: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceStatusSnapshot {
    pub state: MarketplaceSyncState,
    pub consent: MarketplaceConsentDecision,
    pub consent_source: String,
    pub prompt_required: bool,
    pub airgapped: bool,
    pub hub_url: String,
    pub default_hub_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub feed_offer_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offers_preview: Vec<MarketplaceOfferSummary>,
}

#[derive(Debug, Clone)]
pub struct MarketplaceConfig {
    pub mode: DeploymentMode,
    pub default_hub_url: String,
    pub explicit_hub_url: Option<String>,
    pub airgapped: bool,
}

impl MarketplaceConfig {
    /// Resolve marketplace configuration from CLI `--airgapped` flag and environment variables.
    ///
    /// - `COMPASS_DEFAULT_MARKETPLACE_HUB_URL` (fallback alias `COMPASS_MARKETPLACE_HUB_URL`) overrides `DEFAULT_MARKETPLACE_HUB_URL`.
    /// - `COMPASS_HUB_URL` sets an explicit hub URL (which counts as implicit consent in `local` mode unless `--airgapped`).
    /// - `COMPASS_AIRGAPPED` (`1`/`true`/`yes`) or CLI `--airgapped` disables all outbound marketplace traffic and suppresses the prompt.
    /// - CI (`CI=1`/`true`) and unit tests (`cfg!(test)`) default to air-gapped unless `COMPASS_AIRGAPPED=0`/`false`.
    pub fn from_env(mode: DeploymentMode, cli_airgapped: bool) -> Self {
        let default_hub_url = env_non_empty("COMPASS_DEFAULT_MARKETPLACE_HUB_URL")
            .or_else(|| env_non_empty("COMPASS_MARKETPLACE_HUB_URL"))
            .unwrap_or_else(|| DEFAULT_MARKETPLACE_HUB_URL.to_owned());
        let explicit_hub_url = env_non_empty("COMPASS_HUB_URL");
        let airgapped = if cli_airgapped {
            true
        } else {
            match std::env::var("COMPASS_AIRGAPPED").ok().as_deref().map(str::trim) {
                Some(raw) if is_truthy(raw) => true,
                Some(raw) if is_falsey(raw) => false,
                _ => cfg!(test) || env_truthy("CI"),
            }
        };
        Self {
            mode,
            default_hub_url: default_hub_url.trim_end_matches('/').to_owned(),
            explicit_hub_url: explicit_hub_url.map(|u| u.trim_end_matches('/').to_owned()),
            airgapped,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedMarketplaceConsent {
    decision: MarketplaceConsentDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hub_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decided_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
struct MarketplaceRuntimeState {
    decision: MarketplaceConsentDecision,
    consent_source: String,
    hub_url: String,
    decided_at: Option<DateTime<Utc>>,
    last_synced_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    feed_offer_count: usize,
    offers_preview: Vec<MarketplaceOfferSummary>,
}

pub struct MarketplaceClient {
    data_dir: PathBuf,
    cfg: MarketplaceConfig,
    http: Client,
    inner: RwLock<MarketplaceRuntimeState>,
}

impl MarketplaceClient {
    /// Initialize marketplace state from `<data_dir>/marketplace_consent.json`.
    ///
    /// **Invariant (`[A1]`):** Never performs any outbound network request during initialization.
    pub fn new(data_dir: impl AsRef<Path>, cfg: MarketplaceConfig) -> CompassResult<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(15))
            .user_agent(format!("biocompass/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| CompassError::Internal(format!("build marketplace HTTP client: {e}")))?;

        let persisted = load_persisted_consent(&data_dir);
        let state = compute_initial_state(&cfg, persisted);

        Ok(Self {
            data_dir,
            cfg,
            http,
            inner: RwLock::new(state),
        })
    }

    pub fn is_airgapped(&self) -> bool {
        self.cfg.airgapped
    }

    /// Return the current connection/sync status snapshot without triggering any network I/O.
    pub async fn status(&self) -> MarketplaceStatusSnapshot {
        let guard = self.inner.read().await;
        self.snapshot_from_guard(&guard)
    }

    /// Persist user consent choice (`accepted` or `declined`) and optional hub URL override.
    ///
    /// Refused in `--airgapped` and `cloud-web` modes with zero outbound traffic.
    pub async fn set_consent(
        &self,
        decision: MarketplaceConsentDecision,
        hub_url: Option<String>,
        sync_immediately: bool,
    ) -> CompassResult<MarketplaceStatusSnapshot> {
        if self.cfg.airgapped {
            return Err(CompassError::AccessDenied(
                "marketplace connections are disabled in --airgapped mode".into(),
            ));
        }
        if self.cfg.mode != DeploymentMode::Local {
            return Err(CompassError::AccessDenied(
                "marketplace consent settings are managed locally; in cloud-web mode configure federation peers via PlatformAdmin".into(),
            ));
        }
        if decision == MarketplaceConsentDecision::Unset {
            return Err(CompassError::Validation(
                "consent decision must be 'accepted' or 'declined'".into(),
            ));
        }

        let resolved_url = match hub_url.map(|u| u.trim().to_owned()).filter(|u| !u.is_empty()) {
            Some(custom) => validate_marketplace_hub_url(&custom)?,
            None => {
                let guard = self.inner.read().await;
                let candidate = if guard.hub_url.is_empty() {
                    self.cfg.default_hub_url.clone()
                } else {
                    guard.hub_url.clone()
                };
                if decision == MarketplaceConsentDecision::Accepted {
                    validate_marketplace_hub_url(&candidate)?
                } else {
                    candidate
                }
            }
        };

        let now = Utc::now();
        let persisted = PersistedMarketplaceConsent {
            decision,
            hub_url: Some(resolved_url.clone()),
            decided_at: Some(now),
        };
        save_persisted_consent(&self.data_dir, &persisted)?;

        {
            let mut guard = self.inner.write().await;
            guard.decision = decision;
            guard.consent_source = "user_setting".to_owned();
            guard.hub_url = resolved_url;
            guard.decided_at = Some(now);
            if decision == MarketplaceConsentDecision::Declined {
                guard.last_error = None;
                guard.feed_offer_count = 0;
                guard.offers_preview.clear();
            }
        }

        if decision == MarketplaceConsentDecision::Accepted && sync_immediately {
            return self.sync_now().await;
        }
        Ok(self.status().await)
    }

    /// Manually trigger a marketplace feed sync (`GET <hub_url>/v1/feed`).
    ///
    /// Strictly refuses with `AccessDenied` (and zero outbound traffic) when `--airgapped`
    /// or when local first-run consent has not been accepted (`[A1]`).
    pub async fn sync_now(&self) -> CompassResult<MarketplaceStatusSnapshot> {
        if self.cfg.airgapped {
            return Err(CompassError::AccessDenied(
                "marketplace sync is disabled in --airgapped mode".into(),
            ));
        }
        if self.cfg.mode != DeploymentMode::Local {
            return Err(CompassError::AccessDenied(
                "in cloud-web mode, use PlatformAdmin federation peer sync".into(),
            ));
        }

        let hub_url = {
            let guard = self.inner.read().await;
            if guard.decision != MarketplaceConsentDecision::Accepted {
                return Err(CompassError::AccessDenied(
                    "marketplace sync requires explicit first-run opt-in consent".into(),
                ));
            }
            guard.hub_url.clone()
        };

        let validated_hub_url = validate_marketplace_hub_url(&hub_url)?;
        let feed_url = format!("{validated_hub_url}/v1/feed");

        match fetch_marketplace_feed(&self.http, &feed_url).await {
            Ok((count, preview)) => {
                let mut guard = self.inner.write().await;
                guard.last_synced_at = Some(Utc::now());
                guard.last_error = None;
                guard.feed_offer_count = count;
                guard.offers_preview = preview;
                Ok(self.snapshot_from_guard(&guard))
            }
            Err(err) => {
                let mut guard = self.inner.write().await;
                guard.last_error = Some(err.to_string());
                Ok(self.snapshot_from_guard(&guard))
            }
        }
    }

    fn snapshot_from_guard(&self, guard: &MarketplaceRuntimeState) -> MarketplaceStatusSnapshot {
        let prompt_required = !self.cfg.airgapped
            && self.cfg.mode == DeploymentMode::Local
            && guard.decision == MarketplaceConsentDecision::Unset;

        let state = if self.cfg.airgapped {
            MarketplaceSyncState::AirGapped
        } else if self.cfg.mode == DeploymentMode::CloudWeb {
            MarketplaceSyncState::Connected
        } else if guard.decision != MarketplaceConsentDecision::Accepted {
            MarketplaceSyncState::Disconnected
        } else if let Some(last_synced) = guard.last_synced_at {
            let expired = Utc::now().signed_duration_since(last_synced)
                > ChronoDuration::seconds(STALE_AFTER_SECS);
            if expired || guard.last_error.is_some() {
                MarketplaceSyncState::Stale
            } else {
                MarketplaceSyncState::Connected
            }
        } else {
            MarketplaceSyncState::Disconnected
        };

        MarketplaceStatusSnapshot {
            state,
            consent: guard.decision,
            consent_source: guard.consent_source.clone(),
            prompt_required,
            airgapped: self.cfg.airgapped,
            hub_url: guard.hub_url.clone(),
            default_hub_url: self.cfg.default_hub_url.clone(),
            decided_at: guard.decided_at,
            last_synced_at: guard.last_synced_at,
            last_error: guard.last_error.clone(),
            feed_offer_count: guard.feed_offer_count,
            offers_preview: guard.offers_preview.clone(),
        }
    }
}

fn compute_initial_state(
    cfg: &MarketplaceConfig,
    persisted: Option<PersistedMarketplaceConsent>,
) -> MarketplaceRuntimeState {
    if cfg.airgapped {
        return MarketplaceRuntimeState {
            decision: MarketplaceConsentDecision::Declined,
            consent_source: "airgapped".to_owned(),
            hub_url: cfg
                .explicit_hub_url
                .clone()
                .unwrap_or_else(|| cfg.default_hub_url.clone()),
            decided_at: None,
            last_synced_at: None,
            last_error: None,
            feed_offer_count: 0,
            offers_preview: Vec::new(),
        };
    }

    if cfg.mode == DeploymentMode::CloudWeb {
        return MarketplaceRuntimeState {
            decision: MarketplaceConsentDecision::Accepted,
            consent_source: "cloud_web".to_owned(),
            hub_url: cfg
                .explicit_hub_url
                .clone()
                .unwrap_or_else(|| cfg.default_hub_url.clone()),
            decided_at: None,
            last_synced_at: None,
            last_error: None,
            feed_offer_count: 0,
            offers_preview: Vec::new(),
        };
    }

    if let Some(saved) = persisted
        && saved.decision != MarketplaceConsentDecision::Unset
    {
        return MarketplaceRuntimeState {
            decision: saved.decision,
            consent_source: "user_setting".to_owned(),
            hub_url: saved
                .hub_url
                .filter(|u| !u.trim().is_empty())
                .or_else(|| cfg.explicit_hub_url.clone())
                .unwrap_or_else(|| cfg.default_hub_url.clone()),
            decided_at: saved.decided_at,
            last_synced_at: None,
            last_error: None,
            feed_offer_count: 0,
            offers_preview: Vec::new(),
        };
    }

    if let Some(explicit_url) = &cfg.explicit_hub_url {
        return MarketplaceRuntimeState {
            decision: MarketplaceConsentDecision::Accepted,
            consent_source: "env_hub_url".to_owned(),
            hub_url: explicit_url.clone(),
            decided_at: None,
            last_synced_at: None,
            last_error: None,
            feed_offer_count: 0,
            offers_preview: Vec::new(),
        };
    }

    MarketplaceRuntimeState {
        decision: MarketplaceConsentDecision::Unset,
        consent_source: "unset".to_owned(),
        hub_url: cfg.default_hub_url.clone(),
        decided_at: None,
        last_synced_at: None,
        last_error: None,
        feed_offer_count: 0,
        offers_preview: Vec::new(),
    }
}

/// Validate a marketplace Hub base URL: requires `https://` for remote hosts (`http://` allowed
/// strictly for loopback hosts during local development/testing), forbids userinfo/query/fragment
/// and cloud metadata / link-local addresses.
pub fn validate_marketplace_hub_url(raw: &str) -> CompassResult<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() || trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(CompassError::Validation(
            "marketplace hub URL must be non-empty and contain no whitespace or control characters".into(),
        ));
    }
    let parsed = reqwest::Url::parse(trimmed)
        .map_err(|e| CompassError::Validation(format!("invalid marketplace hub URL: {e}")))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(CompassError::Validation(
            "marketplace hub URL must not contain credentials/userinfo".into(),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(CompassError::Validation(
            "marketplace hub URL must not contain query parameters or fragments".into(),
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| CompassError::Validation("marketplace hub URL must include a host".into()))?;
    let unbracketed = host.trim_start_matches('[').trim_end_matches(']');
    let is_loopback = unbracketed.eq_ignore_ascii_case("localhost")
        || unbracketed
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if let Ok(ip) = unbracketed.parse::<IpAddr>()
        && is_forbidden_metadata_ip(ip)
    {
        return Err(CompassError::Validation(
            "marketplace hub URL must not target link-local or unspecified IP addresses".into(),
        ));
    }
    match parsed.scheme() {
        "https" => {}
        "http" if is_loopback => {}
        "http" => {
            return Err(CompassError::Validation(
                "marketplace hub URL must use https:// for non-loopback hosts".into(),
            ));
        }
        other => {
            return Err(CompassError::Validation(format!(
                "unsupported marketplace hub URL scheme '{other}'"
            )));
        }
    }
    Ok(trimmed.to_owned())
}

fn is_forbidden_metadata_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_forbidden_metadata_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_forbidden_metadata_v4(mapped);
            }
            v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
                || v6 == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254) // AWS IMDSv2 IPv6
        }
    }
}

fn is_forbidden_metadata_v4(v4: Ipv4Addr) -> bool {
    v4.is_unspecified() || v4.is_link_local() || v4.is_broadcast() || v4.is_multicast()
}

async fn fetch_marketplace_feed(
    http: &Client,
    feed_url: &str,
) -> CompassResult<(usize, Vec<MarketplaceOfferSummary>)> {
    let resp = http
        .get(feed_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| CompassError::Internal(format!("marketplace feed request failed: {}", e.without_url())))?;
    if !resp.status().is_success() {
        return Err(CompassError::Internal(format!(
            "marketplace feed returned HTTP {}",
            resp.status().as_u16()
        )));
    }
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            CompassError::Internal(format!("read marketplace feed body: {}", e.without_url()))
        })?;
        if body.len().saturating_add(chunk.len()) > MAX_FEED_BODY_BYTES {
            return Err(CompassError::Validation(format!(
                "marketplace feed exceeded {MAX_FEED_BODY_BYTES} byte safety cap"
            )));
        }
        body.extend_from_slice(&chunk);
    }

    let parsed: Value = serde_json::from_slice(&body)
        .map_err(|e| CompassError::Validation(format!("invalid marketplace feed JSON: {e}")))?;

    let items: &[Value] = if let Some(arr) = parsed.as_array() {
        arr
    } else if let Some(arr) = parsed.get("offers").and_then(Value::as_array) {
        arr
    } else if let Some(arr) = parsed.get("items").and_then(Value::as_array) {
        arr
    } else if let Some(arr) = parsed.get("entries").and_then(Value::as_array) {
        arr
    } else {
        &[]
    };

    let preview = items
        .iter()
        .take(MAX_OFFERS_PREVIEW)
        .enumerate()
        .map(|(idx, item)| {
            let id = item
                .get("offer_id")
                .or_else(|| item.get("id"))
                .or_else(|| item.get("plugin_id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("offer-{}", idx + 1));
            let kind = item
                .get("kind")
                .or_else(|| item.get("offer_kind"))
                .or_else(|| item.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("module")
                .to_owned();
            let title = item
                .get("title")
                .or_else(|| item.get("name"))
                .or_else(|| item.get("plugin_id"))
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_owned();
            let version = item
                .get("version")
                .or_else(|| item.get("release_version"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let provider_org = item
                .get("provider_org")
                .or_else(|| item.get("serving_org_id"))
                .or_else(|| item.get("org_id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            MarketplaceOfferSummary {
                id,
                kind,
                title,
                version,
                provider_org,
            }
        })
        .collect();

    Ok((items.len(), preview))
}

fn load_persisted_consent(data_dir: &Path) -> Option<PersistedMarketplaceConsent> {
    let path = data_dir.join(CONSENT_FILE_NAME);
    let raw = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<PersistedMarketplaceConsent>(&raw) {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "ignoring malformed marketplace_consent.json");
            None
        }
    }
}

fn save_persisted_consent(
    data_dir: &Path,
    consent: &PersistedMarketplaceConsent,
) -> CompassResult<()> {
    std::fs::create_dir_all(data_dir)
        .map_err(|e| CompassError::Storage(format!("create data_dir {}: {e}", data_dir.display())))?;
    let encoded = serde_json::to_string_pretty(consent)
        .map_err(|e| CompassError::Internal(format!("encode marketplace consent: {e}")))?;
    let mut tmp = tempfile::NamedTempFile::new_in(data_dir)
        .map_err(|e| CompassError::Storage(format!("create temp consent file: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| CompassError::Storage(format!("chmod temp consent file: {e}")))?;
    }
    tmp.write_all(encoded.as_bytes())
        .map_err(|e| CompassError::Storage(format!("write temp consent file: {e}")))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| CompassError::Storage(format!("sync temp consent file: {e}")))?;
    let dest = data_dir.join(CONSENT_FILE_NAME);
    tmp.persist(&dest)
        .map_err(|e| CompassError::Storage(format!("persist {}: {}", dest.display(), e.error)))?;
    Ok(())
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn env_truthy(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .is_some_and(|v| is_truthy(v.trim()))
}

fn is_truthy(raw: &str) -> bool {
    matches!(raw.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

fn is_falsey(raw: &str) -> bool {
    matches!(raw.to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn spawn_counting_feed_server() -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_bg = Arc::clone(&hits);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                hits_bg.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let body = r#"{"offers":[{"id":"offer-1","kind":"run","title":"HLA Motif Enrichment","version":"1.0.0","provider_org":"org-alithea"}]}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (format!("http://{addr}"), hits)
    }

    #[tokio::test]
    async fn a1_fresh_local_data_dir_makes_zero_outbound_marketplace_requests_before_consent() {
        let (hub_url, hits) = spawn_counting_feed_server().await;
        let tmp = tempfile::tempdir().expect("tempdir");

        let cfg = MarketplaceConfig {
            mode: DeploymentMode::Local,
            default_hub_url: hub_url.clone(),
            explicit_hub_url: None,
            airgapped: false,
        };
        let client = MarketplaceClient::new(tmp.path(), cfg.clone()).expect("client");

        // Fresh local dir: prompt required, disconnected, ZERO outbound hits.
        let status = client.status().await;
        assert!(status.prompt_required);
        assert_eq!(status.state, MarketplaceSyncState::Disconnected);
        assert_eq!(status.consent, MarketplaceConsentDecision::Unset);
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        // Manual sync before consent must fail closed with zero outbound traffic.
        let err = client.sync_now().await.expect_err("sync before consent must fail");
        assert!(matches!(err, CompassError::AccessDenied(_)));
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        // Declining persists across restarts and keeps outbound traffic at zero.
        let declined = client
            .set_consent(MarketplaceConsentDecision::Declined, None, true)
            .await
            .expect("decline");
        assert!(!declined.prompt_required);
        assert_eq!(declined.consent, MarketplaceConsentDecision::Declined);
        assert_eq!(declined.state, MarketplaceSyncState::Disconnected);
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        // Simulate app restart on same data_dir.
        let restarted = MarketplaceClient::new(tmp.path(), cfg.clone()).expect("restart");
        let restart_status = restarted.status().await;
        assert!(!restart_status.prompt_required);
        assert_eq!(restart_status.consent, MarketplaceConsentDecision::Declined);
        assert_eq!(restart_status.state, MarketplaceSyncState::Disconnected);
        assert!(restarted.sync_now().await.is_err());
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        // Only after explicit acceptance does a single GET /v1/feed occur.
        let accepted = restarted
            .set_consent(MarketplaceConsentDecision::Accepted, None, true)
            .await
            .expect("accept");
        assert_eq!(accepted.consent, MarketplaceConsentDecision::Accepted);
        assert_eq!(accepted.state, MarketplaceSyncState::Connected);
        assert_eq!(accepted.feed_offer_count, 1);
        assert!(accepted.last_synced_at.is_some());
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a1_airgapped_mode_suppresses_prompt_and_blocks_all_outbound_marketplace_requests() {
        let (hub_url, hits) = spawn_counting_feed_server().await;
        let tmp = tempfile::tempdir().expect("tempdir");

        // Even with explicit COMPASS_HUB_URL, --airgapped suppresses prompt and blocks ALL requests.
        let cfg = MarketplaceConfig {
            mode: DeploymentMode::Local,
            default_hub_url: hub_url.clone(),
            explicit_hub_url: Some(hub_url),
            airgapped: true,
        };
        let client = MarketplaceClient::new(tmp.path(), cfg).expect("client");

        let status = client.status().await;
        assert!(status.airgapped);
        assert!(!status.prompt_required);
        assert_eq!(status.state, MarketplaceSyncState::AirGapped);
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        assert!(matches!(
            client
                .set_consent(MarketplaceConsentDecision::Accepted, None, true)
                .await,
            Err(CompassError::AccessDenied(_))
        ));
        assert!(matches!(
            client.sync_now().await,
            Err(CompassError::AccessDenied(_))
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a1_explicit_hub_url_counts_as_consent_when_not_airgapped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = MarketplaceConfig {
            mode: DeploymentMode::Local,
            default_hub_url: DEFAULT_MARKETPLACE_HUB_URL.to_owned(),
            explicit_hub_url: Some("https://hub.example.org".to_owned()),
            airgapped: false,
        };
        let client = MarketplaceClient::new(tmp.path(), cfg).expect("client");
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let status = rt.block_on(client.status());
        assert!(!status.prompt_required);
        assert_eq!(status.consent, MarketplaceConsentDecision::Accepted);
        assert_eq!(status.consent_source, "env_hub_url");
        assert_eq!(status.hub_url, "https://hub.example.org");
    }

    #[test]
    fn validate_marketplace_hub_url_enforces_https_and_blocks_metadata_ips() {
        assert!(validate_marketplace_hub_url(DEFAULT_MARKETPLACE_HUB_URL).is_ok());
        assert!(validate_marketplace_hub_url("http://127.0.0.1:8081").is_ok());
        assert!(validate_marketplace_hub_url("http://localhost:8081").is_ok());
        assert!(validate_marketplace_hub_url("http://example.com").is_err());
        assert!(validate_marketplace_hub_url("https://169.254.169.254").is_err());
        assert!(validate_marketplace_hub_url("https://user:pass@hub.example.com").is_err());
        assert!(validate_marketplace_hub_url("https://hub.example.com?foo=bar").is_err());
    }
}
