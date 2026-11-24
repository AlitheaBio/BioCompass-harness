//! Opt-in Global Default Marketplace client, persisted first-run consent (`[A1]`),
//! in-app Workspace Hub Chooser (`U3`), `--airgapped` enforcement, and sync status tracking (`DEV-07`).

use crate::cloud_client::CallerAuth;
use crate::state::DeploymentMode;
use biocompass_core_traits::{
    CompassError, CompassResult, ModuleRunsIn, ModuleSource, PluginId,
};
use biocompass_plugin_spec::{
    NetworkPolicy, PluginExecutionPlane, PluginInputs, PluginManifest, PluginResources,
    PluginUiSpec, SkillDoc,
};
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
const WORKSPACE_HUB_FILE_NAME: &str = "workspace_hub.json";
const STALE_AFTER_SECS: i64 = 15 * 60;
const MAX_FEED_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_OFFERS_PREVIEW: usize = 50;
const MAX_RECENT_HUBS: usize = 8;
const UI_BUNDLE_HASH_CAP_PREFIX: &str = "biocompass.ui_bundle_hash.v1:";
/// The connected hub's module list is reused for this long instead of two outbound fetches per list call (`U-2`).
const REMOTE_MODULES_TTL: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketplaceConsentDecision {
    Unset,
    Accepted,
    Declined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceHubMode {
    Offline,
    AlitheaCloud,
    CustomCloud,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_bundle_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_bundle_size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_milli_act: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The signed `froglet/v1` offer document this summary was parsed from (never serialized).
    #[serde(skip)]
    pub signed_offer: Option<Value>,
}

/// Network offer explicitly subscribed ("Add" button, `U2`) in a local workspace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubscribedNetworkOffer {
    pub offer_id: String,
    pub hub_url: String,
    pub plugin_id: String,
    pub version: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// `"run"` or `"download"`.
    pub kind: String,
    pub provider_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_bundle_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_bundle_size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_url: Option<String>,
    #[serde(default)]
    pub price_milli_act: i64,
}

/// Status snapshot for the in-app Workspace Hub Chooser (`U3`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceHubStatus {
    pub mode: WorkspaceHubMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub_url: Option<String>,
    pub default_hub_url: String,
    pub connected: bool,
    pub airgapped: bool,
    pub authenticated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_actor: Option<String>,
    pub remote_module_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_hubs: Vec<String>,
    /// Pinned hub signing key for the connected hub (`U-1`); compare it with the hub operator's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub_key: Option<String>,
    /// Key the hub presented on the last connection, when it differs from the pinned key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presented_hub_key: Option<String>,
    /// `unpinned`, `pinned` or `changed` (a changed key is refused until re-trusted).
    pub hub_key_status: String,
}

/// Module discovered from a connected cloud hub (`U3`) or subscribed network offer (`U2`).
#[derive(Debug, Clone)]
pub struct RemoteHubModule {
    pub manifest: PluginManifest,
    pub skill: Option<SkillDoc>,
    pub source: ModuleSource,
    pub runs_in: ModuleRunsIn,
    pub has_ui_bundle: bool,
    pub has_remote_icon: bool,
    pub ui_bundle_hash: Option<String>,
    pub ui_bundle_size_bytes: Option<u64>,
    pub can_install: bool,
    pub provider_label: Option<String>,
    pub launch_url: Option<String>,
    pub offer_id: Option<String>,
    pub hub_origin: String,
    /// Verified UI attestation (`U-1`); without it the module's UI is not opened.
    pub ui_attestation: Option<crate::ui_cache::VerifiedUiAttestation>,
    /// Origin the UI bundle is fetched from (the providing peer for federated modules).
    pub ui_fetch_origin: String,
    /// Why a listed UI bundle was refused, if it was.
    pub ui_attestation_error: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedWorkspaceHub {
    mode: WorkspaceHubMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hub_url: Option<String>,
    #[serde(default)]
    recent_hubs: Vec<String>,
    #[serde(default)]
    subscribed_network_offers: Vec<SubscribedNetworkOffer>,
    /// Hub URL -> pinned hub signing key (`U-1`).
    #[serde(default)]
    pinned_hub_keys: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct MarketplaceRuntimeState {
    decision: MarketplaceConsentDecision,
    consent_source: String,
    hub_mode: WorkspaceHubMode,
    hub_url: String,
    decided_at: Option<DateTime<Utc>>,
    last_synced_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    feed_offer_count: usize,
    offers_preview: Vec<MarketplaceOfferSummary>,
    recent_hubs: Vec<String>,
    subscribed_network_offers: Vec<SubscribedNetworkOffer>,
    hub_session_bearer: Option<String>,
    hub_session_actor: Option<String>,
    cached_remote_modules: Vec<RemoteHubModule>,
    cached_remote_at: Option<std::time::Instant>,
    pinned_hub_keys: std::collections::BTreeMap<String, String>,
    presented_hub_key: Option<String>,
}

pub struct MarketplaceClient {
    data_dir: PathBuf,
    cfg: MarketplaceConfig,
    http: Client,
    inner: RwLock<MarketplaceRuntimeState>,
}

impl MarketplaceClient {
    /// Initialize marketplace and workspace-hub state from `<data_dir>`.
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
        let persisted_hub = load_persisted_workspace_hub(&data_dir);
        let state = compute_initial_state(&cfg, persisted, persisted_hub);

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

    /// Return the current in-app Workspace Hub Chooser snapshot (`U3`) without triggering network I/O.
    pub async fn workspace_hub_status(&self) -> WorkspaceHubStatus {
        let guard = self.inner.read().await;
        self.workspace_hub_status_from_guard(&guard)
    }

    fn workspace_hub_status_from_guard(&self, guard: &MarketplaceRuntimeState) -> WorkspaceHubStatus {
        let active_url = if self.cfg.airgapped || guard.hub_mode == WorkspaceHubMode::Offline {
            None
        } else {
            Some(guard.hub_url.clone())
        };
        let connected = active_url.is_some()
            && guard.decision == MarketplaceConsentDecision::Accepted
            && guard.last_error.is_none();
        let pinned = active_url
            .as_ref()
            .and_then(|u| guard.pinned_hub_keys.get(u).cloned());
        let changed = guard
            .presented_hub_key
            .as_ref()
            .is_some_and(|p| pinned.as_ref().is_some_and(|k| k != p));
        let hub_key_status = match (&pinned, changed) {
            (_, true) => "changed",
            (Some(_), false) => "pinned",
            (None, _) => "unpinned",
        }
        .to_owned();
        WorkspaceHubStatus {
            hub_key: pinned,
            presented_hub_key: changed.then(|| guard.presented_hub_key.clone()).flatten(),
            hub_key_status,
            mode: if self.cfg.airgapped {
                WorkspaceHubMode::Offline
            } else {
                guard.hub_mode
            },
            hub_url: active_url,
            default_hub_url: self.cfg.default_hub_url.clone(),
            connected,
            airgapped: self.cfg.airgapped,
            authenticated: guard.hub_session_bearer.is_some(),
            auth_actor: guard.hub_session_actor.clone(),
            remote_module_count: guard.cached_remote_modules.len(),
            last_error: guard.last_error.clone(),
            recent_hubs: guard.recent_hubs.clone(),
        }
    }

    /// Return the currently selected remote hub URL and in-memory auth (if any) in `local` mode.
    pub async fn active_hub_origin_and_auth(&self) -> Option<(String, Option<CallerAuth>)> {
        if self.cfg.airgapped || self.cfg.mode != DeploymentMode::Local {
            return None;
        }
        let guard = self.inner.read().await;
        if guard.hub_mode == WorkspaceHubMode::Offline
            || guard.decision != MarketplaceConsentDecision::Accepted
            || guard.hub_url.is_empty()
        {
            return None;
        }
        let auth = guard.hub_session_bearer.as_ref().map(|token| CallerAuth {
            cookie: None,
            authorization: Some(format!("Bearer {token}")),
            api_key: None,
        });
        Some((guard.hub_url.clone(), auth))
    }

    /// Switch the local client's connected cloud workspace hub in-app (`U3`):
    /// Alithea Cloud, a custom Customer Cloud URL, or Offline.
    ///
    /// Persisted in `<data_dir>/workspace_hub.json`. Switching to a different hub immediately clears
    /// any in-memory session token belonging to the previous hub so credentials never cross hubs.
    pub async fn set_workspace_hub(
        &self,
        mode: WorkspaceHubMode,
        custom_hub_url: Option<String>,
        auth_bearer: Option<String>,
        trusted_hub_key: Option<String>,
    ) -> CompassResult<WorkspaceHubStatus> {
        let trusted_hub_key = trusted_hub_key
            .map(|k| k.trim().to_ascii_lowercase())
            .filter(|k| !k.is_empty());
        if let Some(key) = &trusted_hub_key
            && (key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(CompassError::Validation(
                "hub_key must be the hub's 64-hex-character signing key".into(),
            ));
        }
        if self.cfg.mode != DeploymentMode::Local {
            return Err(CompassError::AccessDenied(
                "workspace hub switcher is only available in local workstation mode".into(),
            ));
        }
        if self.cfg.airgapped && mode != WorkspaceHubMode::Offline {
            return Err(CompassError::AccessDenied(
                "remote hub connections are disabled in --airgapped mode".into(),
            ));
        }

        let resolved_url = match mode {
            WorkspaceHubMode::Offline => None,
            WorkspaceHubMode::AlitheaCloud => {
                Some(validate_marketplace_hub_url(&self.cfg.default_hub_url)?)
            }
            WorkspaceHubMode::CustomCloud => {
                let raw = custom_hub_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        CompassError::Validation(
                            "custom_cloud mode requires a valid hub_url".into(),
                        )
                    })?;
                Some(validate_marketplace_hub_url(raw)?)
            }
        };

        let now = Utc::now();
        {
            let mut guard = self.inner.write().await;
            let previous_url = guard.hub_url.clone();
            guard.hub_mode = mode;

            match &resolved_url {
                None => {
                    guard.decision = MarketplaceConsentDecision::Declined;
                    guard.consent_source = "user_setting".to_owned();
                    guard.decided_at = Some(now);
                    guard.hub_session_bearer = None;
                    guard.hub_session_actor = None;
                    guard.last_error = None;
                    guard.feed_offer_count = 0;
                    guard.offers_preview.clear();
                    guard.cached_remote_modules.clear();
                }
                Some(url) => {
                    if previous_url != *url {
                        // Never carry an auth token from one hub to another.
                        guard.hub_session_bearer = None;
                        guard.hub_session_actor = None;
                        guard.cached_remote_modules.clear();
                        guard.offers_preview.clear();
                        guard.feed_offer_count = 0;
                    }
                    if let Some(token) = auth_bearer
                        .as_deref()
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                    {
                        guard.hub_session_bearer = Some(token.to_owned());
                    }
                    if let Some(key) = &trusted_hub_key {
                        // Explicit entry or re-trust after a key change.
                        guard.pinned_hub_keys.insert(url.clone(), key.clone());
                        guard.presented_hub_key = None;
                        guard.cached_remote_modules.clear();
                        guard.cached_remote_at = None;
                    }
                    if previous_url != *url {
                        guard.presented_hub_key = None;
                        guard.cached_remote_at = None;
                    }
                    guard.hub_url = url.clone();
                    guard.decision = MarketplaceConsentDecision::Accepted;
                    guard.consent_source = "user_setting".to_owned();
                    guard.decided_at = Some(now);
                    guard.last_error = None;
                    push_recent_hub(&mut guard.recent_hubs, url);
                }
            }

            let persisted_hub = PersistedWorkspaceHub {
                mode: guard.hub_mode,
                hub_url: resolved_url.clone(),
                recent_hubs: guard.recent_hubs.clone(),
                subscribed_network_offers: guard.subscribed_network_offers.clone(),
                pinned_hub_keys: guard.pinned_hub_keys.clone(),
            };
            save_persisted_workspace_hub(&self.data_dir, &persisted_hub)?;

            let persisted_consent = PersistedMarketplaceConsent {
                decision: guard.decision,
                hub_url: Some(guard.hub_url.clone()),
                decided_at: Some(now),
            };
            save_persisted_consent(&self.data_dir, &persisted_consent)?;
        }

        if resolved_url.is_some() {
            let _ = self.refresh_connected_hub().await;
        }
        Ok(self.workspace_hub_status().await)
    }

    /// Sign into the currently selected cloud workspace hub (`POST <hub_url>/api/v1/auth/login`).
    pub async fn login_workspace_hub(
        &self,
        login_body: Value,
    ) -> CompassResult<WorkspaceHubStatus> {
        let (hub_url, _) = self.active_hub_origin_and_auth().await.ok_or_else(|| {
            CompassError::Validation(
                "select Alithea Cloud or a Customer Cloud URL before signing in".into(),
            )
        })?;
        let url = format!("{hub_url}/api/v1/auth/login");
        let resp = self
            .http
            .post(&url)
            .json(&login_body)
            .send()
            .await
            .map_err(|e| {
                CompassError::Storage(format!(
                    "hub login request failed: {}",
                    e.without_url()
                ))
            })?;
        if !resp.status().is_success() {
            return Err(crate::cloud_client::map_http_error(resp).await);
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| CompassError::Protocol(format!("decode hub login JSON: {e}")))?;
        let token = body
            .get("session_token")
            .or_else(|| body.get("token"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                CompassError::Protocol("hub login response omitted session_token".into())
            })?
            .to_owned();
        let actor = body
            .get("email")
            .or_else(|| body.get("actor"))
            .or_else(|| body.pointer("/principal/email"))
            .or_else(|| body.pointer("/principal/actor"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        {
            let mut guard = self.inner.write().await;
            if guard.hub_url == hub_url {
                guard.hub_session_bearer = Some(token);
                guard.hub_session_actor = actor;
            }
        }
        let _ = self.refresh_connected_hub().await;
        Ok(self.workspace_hub_status().await)
    }

    /// Clear the in-memory session token for the connected cloud workspace hub.
    pub async fn logout_workspace_hub(&self) -> WorkspaceHubStatus {
        {
            let mut guard = self.inner.write().await;
            guard.hub_session_bearer = None;
            guard.hub_session_actor = None;
        }
        self.workspace_hub_status().await
    }

    /// Refresh feed + remote module catalog from the currently connected hub (`U3`).
    ///
    /// `U-2`: only the stored hub session token is ever sent to the hub; the browser's local
    /// `Cookie` / `Authorization` / `X-API-Key` never leave this machine.
    /// `U-1`: the hub key is pinned on first connection; a changed key is refused, and every
    /// module's UI attestation is verified against the pinned key before it can be opened.
    pub async fn refresh_connected_hub(&self) -> CompassResult<Vec<RemoteHubModule>> {
        let Some((hub_url, stored_auth)) = self.active_hub_origin_and_auth().await else {
            return Ok(Vec::new());
        };
        let validated_hub_url = validate_marketplace_hub_url(&hub_url)?;

        // 1. Pin (first connection) or check the hub's signing key.
        let pinned_key = self.check_hub_key(&validated_hub_url).await?;

        // 2. Feed, org modules and federated network modules, fetched concurrently.
        let feed_url = format!("{validated_hub_url}/v1/feed");
        let (feed_result, plugins_result, network_result) = tokio::join!(
            fetch_marketplace_feed(&self.http, &feed_url),
            fetch_hub_plugins(&self.http, &validated_hub_url, stored_auth.as_ref()),
            fetch_network_modules(&self.http, &validated_hub_url, stored_auth.as_ref()),
        );

        let mut modules: Vec<RemoteHubModule> = Vec::new();
        let mut seen_ids = std::collections::HashSet::<String>::new();

        if let Ok(org_plugins) = &plugins_result {
            for (m, attestation_doc) in org_plugins {
                let mut m = m.clone();
                attach_attestation(&mut m, pinned_key.as_deref(), attestation_doc.as_ref(), None);
                seen_ids.insert(m.manifest.id.as_str().to_owned());
                modules.push(m);
            }
        }
        if let Ok(network) = &network_result {
            for entry in network {
                if let Some(mut m) = entry.to_module(&validated_hub_url) {
                    attach_attestation(
                        &mut m,
                        pinned_key.as_deref(),
                        entry.module_attestation.as_ref(),
                        entry.froglet_offer.as_ref(),
                    );
                    if seen_ids.insert(m.manifest.id.as_str().to_owned()) {
                        modules.push(m);
                    }
                }
            }
        }

        let mut last_error: Option<String> = None;
        let (feed_count, feed_offers) = match feed_result {
            Ok((count, offers)) => (count, offers),
            Err(err) => {
                if plugins_result.is_err() {
                    last_error = Some(err.to_string());
                }
                (0, Vec::new())
            }
        };

        // Include public module offers from the connected hub's feed if not already listed.
        // The hub signs its own listings, so a feed offer is attested when it is signed by the
        // pinned hub key.
        for offer in &feed_offers {
            if !matches!(offer.kind.as_str(), "run" | "download") {
                continue;
            }
            if let Some(mut m) = synthesize_module_from_offer(offer, &validated_hub_url)
                && seen_ids.insert(m.manifest.id.as_str().to_owned())
            {
                match (pinned_key.as_deref(), offer.signed_offer.as_ref()) {
                    (Some(key), Some(doc)) => {
                        match crate::ui_cache::VerifiedUiAttestation::from_signed_offer(
                            doc,
                            key,
                            m.manifest.id.as_str(),
                            &m.manifest.version,
                        ) {
                            Ok(a) => m.ui_attestation = Some(a),
                            Err(e) => {
                                m.ui_attestation_error = Some(e.to_string());
                                m.ui_bundle_hash = None;
                            }
                        }
                    }
                    _ => {
                        m.ui_bundle_hash = None;
                    }
                }
                modules.push(m);
            }
        }

        let mut guard = self.inner.write().await;
        if guard.hub_url == validated_hub_url {
            guard.last_synced_at = if last_error.is_none() {
                Some(Utc::now())
            } else {
                guard.last_synced_at
            };
            guard.last_error = last_error;
            if feed_count > 0 || !feed_offers.is_empty() {
                guard.feed_offer_count = feed_count;
                guard.offers_preview = feed_offers;
            }
            guard.cached_remote_modules = modules.clone();
            guard.cached_remote_at = Some(std::time::Instant::now());
        }
        Ok(modules)
    }

    /// Fetch `/v1/hub-identity`, pin it on first connection, and refuse a changed key (`U-1`).
    /// Returns the pinned key, or `None` when the hub publishes no identity and none is pinned.
    async fn check_hub_key(&self, hub_url: &str) -> CompassResult<Option<String>> {
        let presented = fetch_hub_identity(&self.http, hub_url).await.ok();
        let mut guard = self.inner.write().await;
        if guard.hub_url != hub_url {
            return Ok(None);
        }
        let pinned = guard.pinned_hub_keys.get(hub_url).cloned();
        match (pinned, presented) {
            (None, Some(key)) => {
                guard.pinned_hub_keys.insert(hub_url.to_owned(), key.clone());
                guard.presented_hub_key = None;
                let persisted = persisted_from_guard(&guard);
                save_persisted_workspace_hub(&self.data_dir, &persisted)?;
                Ok(Some(key))
            }
            (Some(pinned), Some(key)) if pinned != key => {
                guard.presented_hub_key = Some(key.clone());
                guard.cached_remote_modules.clear();
                guard.cached_remote_at = None;
                let msg = format!(
                    "UI_HUB_KEY_CHANGED: hub {hub_url} now presents key {key}, but {pinned} is pinned; \
                     compare it with the hub operator and re-trust it in the hub chooser"
                );
                guard.last_error = Some(msg.clone());
                Err(CompassError::ui_bundle_tampered(msg))
            }
            (Some(pinned), _) => {
                guard.presented_hub_key = None;
                Ok(Some(pinned))
            }
            (None, None) => Ok(None),
        }
    }

    /// The modules fetched by the last refresh, without any network I/O.
    pub async fn cached_remote_modules(&self) -> Vec<RemoteHubModule> {
        self.inner.read().await.cached_remote_modules.clone()
    }

    /// Return the remote modules (cached for a short TTL) plus subscribed network offers (`U2`/`U3`).
    pub async fn list_remote_and_subscribed_modules(&self) -> Vec<RemoteHubModule> {
        let mut out = if let Some((hub_url, _)) = self.active_hub_origin_and_auth().await {
            let fresh = {
                let guard = self.inner.read().await;
                (guard.hub_url == hub_url
                    && guard
                        .cached_remote_at
                        .is_some_and(|at| at.elapsed() < REMOTE_MODULES_TTL))
                .then(|| guard.cached_remote_modules.clone())
            };
            match fresh {
                Some(mods) => mods,
                None => match self.refresh_connected_hub().await {
                    Ok(mods) => mods,
                    Err(_) => {
                        let guard = self.inner.read().await;
                        if guard.presented_hub_key.is_some() {
                            Vec::new()
                        } else {
                            guard.cached_remote_modules.clone()
                        }
                    }
                },
            }
        } else {
            Vec::new()
        };

        let mut seen: std::collections::HashSet<String> = out
            .iter()
            .map(|m| m.manifest.id.as_str().to_owned())
            .collect();

        let subscribed = self.inner.read().await.subscribed_network_offers.clone();
        for sub in subscribed {
            if let Some(module) = synthesize_module_from_subscription(&sub)
                && seen.insert(module.manifest.id.as_str().to_owned())
            {
                // A subscription's own hash is user-supplied and never trusted (`U-1`): its UI
                // opens only while the connected hub lists the module with a verified attestation.
                out.push(module);
            }
        }
        out
    }

    /// List explicitly subscribed network offers (`Add` button in `U2`).
    pub async fn list_subscribed_network_offers(&self) -> Vec<SubscribedNetworkOffer> {
        self.inner.read().await.subscribed_network_offers.clone()
    }

    /// Subscribe to a network offer (`U2` "Add" button) so it appears in the module list.
    pub async fn subscribe_network_offer(
        &self,
        offer: SubscribedNetworkOffer,
    ) -> CompassResult<Vec<SubscribedNetworkOffer>> {
        if self.cfg.airgapped {
            return Err(CompassError::AccessDenied(
                "subscribing to network offers is disabled in --airgapped mode".into(),
            ));
        }
        if self.cfg.mode != DeploymentMode::Local {
            return Err(CompassError::AccessDenied(
                "network offers are subscribed per local workspace; in cloud-web mode use the organisation's module offers".into(),
            ));
        }
        if self.inner.read().await.decision != MarketplaceConsentDecision::Accepted {
            return Err(CompassError::AccessDenied(
                "MARKETPLACE_CONSENT_REQUIRED: connect to a hub (marketplace consent) before adding network offers".into(),
            ));
        }
        validate_marketplace_hub_url(&offer.hub_url)?;
        PluginId::new(offer.plugin_id.clone()).map_err(CompassError::Validation)?;
        if offer.version.trim().is_empty() {
            return Err(CompassError::Validation("offer version is required".into()));
        }
        biocompass_core_traits::validate_requested_plugin_version(Some(&offer.version))?;
        if let Some(hash) = offer.ui_bundle_hash.as_deref()
            && !crate::ui_cache::is_sha256_hex(hash)
        {
            return Err(CompassError::Validation(
                "offer ui_bundle_hash must be a 64-character lowercase hex SHA-256 digest".into(),
            ));
        }
        if let Some(launch_url) = offer.launch_url.as_deref().filter(|s| !s.trim().is_empty()) {
            validate_marketplace_hub_url(launch_url)?;
        }
        let mut guard = self.inner.write().await;
        if let Some(existing) = guard
            .subscribed_network_offers
            .iter_mut()
            .find(|item| item.offer_id == offer.offer_id || item.plugin_id == offer.plugin_id)
        {
            *existing = offer;
        } else {
            guard.subscribed_network_offers.push(offer);
        }
        let persisted = persisted_from_guard(&guard);
        save_persisted_workspace_hub(&self.data_dir, &persisted)?;
        Ok(guard.subscribed_network_offers.clone())
    }

    /// Remove a subscribed network offer from the local module list.
    pub async fn unsubscribe_network_offer(
        &self,
        offer_id: &str,
    ) -> CompassResult<Vec<SubscribedNetworkOffer>> {
        let mut guard = self.inner.write().await;
        guard
            .subscribed_network_offers
            .retain(|item| item.offer_id != offer_id && item.plugin_id != offer_id);
        let persisted = persisted_from_guard(&guard);
        save_persisted_workspace_hub(&self.data_dir, &persisted)?;
        Ok(guard.subscribed_network_offers.clone())
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
            let url_changed = guard.hub_url != resolved_url;
            guard.decision = decision;
            guard.consent_source = "user_setting".to_owned();
            guard.hub_url = resolved_url.clone();
            guard.decided_at = Some(now);
            if decision == MarketplaceConsentDecision::Declined {
                guard.hub_mode = WorkspaceHubMode::Offline;
                guard.hub_session_bearer = None;
                guard.hub_session_actor = None;
                guard.last_error = None;
                guard.feed_offer_count = 0;
                guard.offers_preview.clear();
                guard.cached_remote_modules.clear();
            } else {
                if url_changed {
                    guard.hub_session_bearer = None;
                    guard.hub_session_actor = None;
                }
                guard.hub_mode = if resolved_url == self.cfg.default_hub_url {
                    WorkspaceHubMode::AlitheaCloud
                } else {
                    WorkspaceHubMode::CustomCloud
                };
                push_recent_hub(&mut guard.recent_hubs, &resolved_url);
            }
            let persisted_hub = PersistedWorkspaceHub {
                mode: guard.hub_mode,
                hub_url: (guard.hub_mode != WorkspaceHubMode::Offline)
                    .then(|| guard.hub_url.clone()),
                recent_hubs: guard.recent_hubs.clone(),
                subscribed_network_offers: guard.subscribed_network_offers.clone(),
                pinned_hub_keys: guard.pinned_hub_keys.clone(),
            };
            let _ = save_persisted_workspace_hub(&self.data_dir, &persisted_hub);
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

fn persisted_from_guard(guard: &MarketplaceRuntimeState) -> PersistedWorkspaceHub {
    PersistedWorkspaceHub {
        mode: guard.hub_mode,
        hub_url: (guard.hub_mode != WorkspaceHubMode::Offline).then(|| guard.hub_url.clone()),
        recent_hubs: guard.recent_hubs.clone(),
        subscribed_network_offers: guard.subscribed_network_offers.clone(),
        pinned_hub_keys: guard.pinned_hub_keys.clone(),
    }
}

async fn fetch_hub_identity(http: &Client, hub_url: &str) -> CompassResult<String> {
    let resp = http
        .get(format!("{hub_url}/v1/hub-identity"))
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| CompassError::Storage(format!("hub identity request failed: {}", e.without_url())))?;
    if !resp.status().is_success() {
        return Err(CompassError::NotFound("hub publishes no identity".into()));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| CompassError::Protocol(format!("decode hub identity: {e}")))?;
    body.get("hub_key")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .filter(|k| k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| CompassError::Protocol("hub identity has no valid hub_key".into()))
}

/// Verify a listed module's attestation and either attach it or drop the UI (`U-1`).
fn attach_attestation(
    m: &mut RemoteHubModule,
    pinned_key: Option<&str>,
    doc: Option<&Value>,
    provider_offer: Option<&Value>,
) {
    let Some(listed_hash) = m.ui_bundle_hash.clone() else {
        return;
    };
    let result = match (pinned_key, doc) {
        (Some(key), Some(doc)) => crate::ui_cache::VerifiedUiAttestation::verify(
            doc,
            key,
            m.manifest.id.as_str(),
            &m.manifest.version,
            provider_offer,
        )
        .and_then(|a| {
            if a.ui_bundle_hash == listed_hash {
                Ok(a)
            } else {
                Err(CompassError::ui_bundle_tampered(
                    "UI_ATTESTATION_INVALID: listed UI hash differs from the attested hash",
                ))
            }
        }),
        (None, _) => Err(CompassError::ui_bundle_tampered(
            "UI_ATTESTATION_INVALID: no pinned hub key for this hub",
        )),
        (_, None) => Err(CompassError::ui_bundle_tampered(
            "UI_ATTESTATION_INVALID: the hub listed this module without a signed attestation",
        )),
    };
    match result {
        Ok(a) => {
            m.ui_bundle_size_bytes = a.ui_bundle_size_bytes.or(m.ui_bundle_size_bytes);
            m.ui_attestation = Some(a);
        }
        Err(e) => {
            tracing::warn!(plugin = %m.manifest.id, error = %e, "refusing unattested module UI");
            m.ui_attestation_error = Some(e.to_string());
            m.ui_bundle_hash = None;
            m.has_ui_bundle = false;
        }
    }
}

#[derive(Debug, Deserialize)]
struct NetworkModulesEnvelope {
    #[serde(default)]
    modules: Vec<NetworkModuleWire>,
}

#[derive(Debug, Clone, Deserialize)]
struct NetworkModuleWire {
    plugin_id: String,
    version: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    offer_id: Option<String>,
    #[serde(default)]
    offer_kind: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    peer_origin: Option<String>,
    #[serde(default)]
    launch_url: Option<String>,
    #[serde(default)]
    ui_bundle_hash: Option<String>,
    #[serde(default)]
    froglet_offer: Option<Value>,
    #[serde(default)]
    module_attestation: Option<Value>,
}

impl NetworkModuleWire {
    fn to_module(&self, hub_origin: &str) -> Option<RemoteHubModule> {
        let kind = match self.offer_kind.as_deref() {
            Some("download") | Some("biocompass.module.download.v1") => "download",
            _ => "run",
        };
        let offer = MarketplaceOfferSummary {
            id: self.offer_id.clone().unwrap_or_else(|| self.plugin_id.clone()),
            kind: kind.to_owned(),
            title: self.title.clone().unwrap_or_else(|| self.plugin_id.clone()),
            version: Some(self.version.clone()),
            provider_org: self.provider.clone(),
            plugin_id: Some(self.plugin_id.clone()),
            ui_bundle_hash: self
                .ui_bundle_hash
                .clone()
                .filter(|h| crate::ui_cache::is_sha256_hex(h)),
            ui_bundle_size_bytes: None,
            launch_url: self.launch_url.clone(),
            price_milli_act: None,
            description: None,
            signed_offer: None,
        };
        let mut m = synthesize_module_from_offer(&offer, hub_origin)?;
        // Federated bundles are fetched from the providing peer and verified against the
        // peer-signed offer; the connected hub only endorses the peer's key.
        if let Some(origin) = self
            .peer_origin
            .as_deref()
            .and_then(|o| validate_marketplace_hub_url(o).ok())
        {
            m.ui_fetch_origin = origin;
        }
        Some(m)
    }
}

async fn fetch_network_modules(
    http: &Client,
    hub_url: &str,
    auth: Option<&CallerAuth>,
) -> CompassResult<Vec<NetworkModuleWire>> {
    let Some(caller) = auth else {
        return Ok(Vec::new());
    };
    let mut req = http
        .get(format!("{hub_url}/api/v1/network-modules"))
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(bearer) = caller.authorization.as_deref().filter(|s| !s.is_empty()) {
        req = req.header("Authorization", bearer);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| CompassError::Storage(format!("fetch network modules failed: {}", e.without_url())))?;
    if !resp.status().is_success() {
        return Err(crate::cloud_client::map_http_error(resp).await);
    }
    let body: NetworkModulesEnvelope = resp
        .json()
        .await
        .map_err(|e| CompassError::Protocol(format!("decode network modules JSON: {e}")))?;
    Ok(body.modules)
}

fn push_recent_hub(list: &mut Vec<String>, url: &str) {
    list.retain(|existing| existing != url);
    list.insert(0, url.to_owned());
    list.truncate(MAX_RECENT_HUBS);
}

fn compute_initial_state(
    cfg: &MarketplaceConfig,
    persisted: Option<PersistedMarketplaceConsent>,
    persisted_hub: Option<PersistedWorkspaceHub>,
) -> MarketplaceRuntimeState {
    let recent_hubs = persisted_hub
        .as_ref()
        .map(|h| h.recent_hubs.clone())
        .unwrap_or_default();
    let subscribed_network_offers = persisted_hub
        .as_ref()
        .map(|h| h.subscribed_network_offers.clone())
        .unwrap_or_default();
    let pinned_hub_keys = persisted_hub
        .as_ref()
        .map(|h| h.pinned_hub_keys.clone())
        .unwrap_or_default();

    if cfg.airgapped {
        return MarketplaceRuntimeState {
            decision: MarketplaceConsentDecision::Declined,
            consent_source: "airgapped".to_owned(),
            hub_mode: WorkspaceHubMode::Offline,
            hub_url: cfg
                .explicit_hub_url
                .clone()
                .unwrap_or_else(|| cfg.default_hub_url.clone()),
            decided_at: None,
            last_synced_at: None,
            last_error: None,
            feed_offer_count: 0,
            offers_preview: Vec::new(),
            recent_hubs,
            subscribed_network_offers,
            hub_session_bearer: None,
            hub_session_actor: None,
            cached_remote_modules: Vec::new(),
            cached_remote_at: None,
            pinned_hub_keys: pinned_hub_keys.clone(),
            presented_hub_key: None,
        };
    }

    if cfg.mode == DeploymentMode::CloudWeb {
        let url = cfg
            .explicit_hub_url
            .clone()
            .unwrap_or_else(|| cfg.default_hub_url.clone());
        return MarketplaceRuntimeState {
            decision: MarketplaceConsentDecision::Accepted,
            consent_source: "cloud_web".to_owned(),
            hub_mode: if url == cfg.default_hub_url {
                WorkspaceHubMode::AlitheaCloud
            } else {
                WorkspaceHubMode::CustomCloud
            },
            hub_url: url,
            decided_at: None,
            last_synced_at: None,
            last_error: None,
            feed_offer_count: 0,
            offers_preview: Vec::new(),
            recent_hubs,
            subscribed_network_offers,
            hub_session_bearer: None,
            hub_session_actor: None,
            cached_remote_modules: Vec::new(),
            cached_remote_at: None,
            pinned_hub_keys: pinned_hub_keys.clone(),
            presented_hub_key: None,
        };
    }

    if let Some(saved_hub) = persisted_hub {
        let decision = match saved_hub.mode {
            WorkspaceHubMode::Offline => MarketplaceConsentDecision::Declined,
            WorkspaceHubMode::AlitheaCloud | WorkspaceHubMode::CustomCloud => {
                MarketplaceConsentDecision::Accepted
            }
        };
        let url = saved_hub
            .hub_url
            .filter(|u| !u.trim().is_empty())
            .or_else(|| cfg.explicit_hub_url.clone())
            .unwrap_or_else(|| cfg.default_hub_url.clone());
        return MarketplaceRuntimeState {
            decision,
            consent_source: "user_setting".to_owned(),
            hub_mode: saved_hub.mode,
            hub_url: url,
            decided_at: persisted.and_then(|p| p.decided_at),
            last_synced_at: None,
            last_error: None,
            feed_offer_count: 0,
            offers_preview: Vec::new(),
            recent_hubs,
            subscribed_network_offers,
            hub_session_bearer: None,
            hub_session_actor: None,
            cached_remote_modules: Vec::new(),
            cached_remote_at: None,
            pinned_hub_keys: pinned_hub_keys.clone(),
            presented_hub_key: None,
        };
    }

    if let Some(saved) = persisted
        && saved.decision != MarketplaceConsentDecision::Unset
    {
        let url = saved
            .hub_url
            .filter(|u| !u.trim().is_empty())
            .or_else(|| cfg.explicit_hub_url.clone())
            .unwrap_or_else(|| cfg.default_hub_url.clone());
        let hub_mode = if saved.decision == MarketplaceConsentDecision::Declined {
            WorkspaceHubMode::Offline
        } else if url == cfg.default_hub_url {
            WorkspaceHubMode::AlitheaCloud
        } else {
            WorkspaceHubMode::CustomCloud
        };
        return MarketplaceRuntimeState {
            decision: saved.decision,
            consent_source: "user_setting".to_owned(),
            hub_mode,
            hub_url: url,
            decided_at: saved.decided_at,
            last_synced_at: None,
            last_error: None,
            feed_offer_count: 0,
            offers_preview: Vec::new(),
            recent_hubs,
            subscribed_network_offers,
            hub_session_bearer: None,
            hub_session_actor: None,
            cached_remote_modules: Vec::new(),
            cached_remote_at: None,
            pinned_hub_keys: pinned_hub_keys.clone(),
            presented_hub_key: None,
        };
    }

    if let Some(explicit_url) = &cfg.explicit_hub_url {
        let hub_mode = if *explicit_url == cfg.default_hub_url {
            WorkspaceHubMode::AlitheaCloud
        } else {
            WorkspaceHubMode::CustomCloud
        };
        return MarketplaceRuntimeState {
            decision: MarketplaceConsentDecision::Accepted,
            consent_source: "env_hub_url".to_owned(),
            hub_mode,
            hub_url: explicit_url.clone(),
            decided_at: None,
            last_synced_at: None,
            last_error: None,
            feed_offer_count: 0,
            offers_preview: Vec::new(),
            recent_hubs,
            subscribed_network_offers,
            hub_session_bearer: None,
            hub_session_actor: None,
            cached_remote_modules: Vec::new(),
            cached_remote_at: None,
            pinned_hub_keys: pinned_hub_keys.clone(),
            presented_hub_key: None,
        };
    }

    MarketplaceRuntimeState {
        decision: MarketplaceConsentDecision::Unset,
        consent_source: "unset".to_owned(),
        hub_mode: WorkspaceHubMode::Offline,
        hub_url: cfg.default_hub_url.clone(),
        decided_at: None,
        last_synced_at: None,
        last_error: None,
        feed_offer_count: 0,
        offers_preview: Vec::new(),
        recent_hubs,
        subscribed_network_offers,
        hub_session_bearer: None,
        hub_session_actor: None,
        cached_remote_modules: Vec::new(),
        cached_remote_at: None,
        pinned_hub_keys,
        presented_hub_key: None,
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

    Ok(parse_marketplace_feed(&parsed))
}

fn classify_froglet_offer_kind(raw: &str) -> String {
    match raw.trim() {
        "biocompass.module.run.v1" => "run".to_owned(),
        "biocompass.module.download.v1" => "download".to_owned(),
        "dataset.ducklake.v1" => "dataset".to_owned(),
        other if other.starts_with("dataset.") || other.starts_with("biocompass.dataset.") => {
            "dataset".to_owned()
        }
        other if !other.is_empty() => other.to_owned(),
        _ => "offer".to_owned(),
    }
}

/// Parse a `GET /v1/feed` JSON response.
///
/// Supports the canonical `compass-hub` Froglet feed wire format (`{ "artifacts": [...],
/// "active_offer_hashes": [...], "has_more", "next_cursor" }`) as well as legacy flat
/// `"offers"` / `"items"` / `"entries"` / array responses.
pub(crate) fn parse_marketplace_feed(parsed: &Value) -> (usize, Vec<MarketplaceOfferSummary>) {
    if let Some(artifacts) = parsed.get("artifacts").and_then(Value::as_array) {
        let active_hashes: Option<std::collections::HashSet<&str>> = parsed
            .get("active_offer_hashes")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_str).collect());
        let has_more = parsed
            .get("has_more")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let mut matched_count = 0usize;
        let mut preview = Vec::new();

        for (idx, entry) in artifacts.iter().enumerate() {
            let artifact_kind = entry
                .get("artifact_kind")
                .or_else(|| entry.pointer("/document/artifact_type"))
                .and_then(Value::as_str);
            if artifact_kind != Some("offer") {
                continue;
            }

            let entry_hash = entry
                .get("artifact_hash")
                .or_else(|| entry.pointer("/document/hash"))
                .and_then(Value::as_str);
            if let Some(active) = &active_hashes
                && !entry_hash.is_some_and(|h| active.contains(h))
            {
                continue;
            }

            matched_count += 1;
            if preview.len() >= MAX_OFFERS_PREVIEW {
                continue;
            }

            let payload = entry.pointer("/document/payload");
            let id = payload
                .and_then(|p| p.get("offer_id"))
                .and_then(Value::as_str)
                .or(entry_hash)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("offer-{}", idx + 1));

            let raw_kind = payload
                .and_then(|p| p.get("offer_kind"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let kind = classify_froglet_offer_kind(raw_kind);

            let access_handle = payload
                .and_then(|p| p.pointer("/execution_profile/access_handles/0"))
                .and_then(Value::as_str);

            let (title, version) = match (kind.as_str(), access_handle) {
                ("run" | "download", Some(handle)) => match handle.split_once('@') {
                    Some((plugin_id, ver)) if !plugin_id.is_empty() && !ver.is_empty() => {
                        (plugin_id.to_owned(), Some(ver.to_owned()))
                    }
                    _ => (handle.to_owned(), None),
                },
                (_, Some(handle)) if !handle.is_empty() => (handle.to_owned(), None),
                _ => (id.clone(), None),
            };

            let provider_org = payload
                .and_then(|p| p.get("provider_id"))
                .and_then(Value::as_str)
                .or_else(|| entry.pointer("/document/signer").and_then(Value::as_str))
                .or_else(|| entry.get("actor_id").and_then(Value::as_str))
                .map(str::to_owned);

            let ui_bundle_hash = payload
                .and_then(|p| p.pointer("/execution_profile/capabilities"))
                .and_then(Value::as_array)
                .and_then(|caps| {
                    caps.iter().filter_map(Value::as_str).find_map(|cap| {
                        cap.strip_prefix(UI_BUNDLE_HASH_CAP_PREFIX)
                            .filter(|h| crate::ui_cache::is_sha256_hex(h))
                            .map(str::to_owned)
                    })
                })
                .or_else(|| {
                    payload
                        .and_then(|p| p.get("ui_bundle_hash"))
                        .and_then(Value::as_str)
                        .filter(|h| crate::ui_cache::is_sha256_hex(h))
                        .map(str::to_owned)
                });
            let ui_bundle_size_bytes = payload
                .and_then(|p| p.get("ui_bundle_size_bytes"))
                .and_then(Value::as_u64);
            let launch_url = payload
                .and_then(|p| p.get("launch_url"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let price_milli_act = payload
                .and_then(|p| p.pointer("/pricing/price_milli_act").or_else(|| p.get("price_milli_act")))
                .and_then(Value::as_i64);
            let plugin_id = match kind.as_str() {
                "run" | "download" if title.contains('/') => Some(title.clone()),
                _ => None,
            };

            preview.push(MarketplaceOfferSummary {
                id,
                kind,
                title,
                version,
                provider_org,
                plugin_id,
                ui_bundle_hash,
                ui_bundle_size_bytes,
                launch_url,
                price_milli_act,
                description: None,
                signed_offer: entry.get("document").cloned(),
            });
        }

        let total_count = if has_more {
            active_hashes
                .as_ref()
                .map_or(matched_count, |active| matched_count.max(active.len()))
        } else {
            matched_count
        };
        return (total_count, preview);
    }

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
                .map(classify_froglet_offer_kind)
                .unwrap_or_else(|| "module".to_owned());
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
            let plugin_id = item
                .get("plugin_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| title.contains('/').then(|| title.clone()));
            let ui_bundle_hash = item
                .get("ui_bundle_hash")
                .and_then(Value::as_str)
                .filter(|h| crate::ui_cache::is_sha256_hex(h))
                .map(str::to_owned);
            let ui_bundle_size_bytes = item.get("ui_bundle_size_bytes").and_then(Value::as_u64);
            let launch_url = item
                .get("launch_url")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let price_milli_act = item.get("price_milli_act").and_then(Value::as_i64);
            let description = item
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned);
            MarketplaceOfferSummary {
                id,
                kind,
                title,
                version,
                provider_org,
                plugin_id,
                ui_bundle_hash,
                ui_bundle_size_bytes,
                launch_url,
                price_milli_act,
                description,
                signed_offer: None,
            }
        })
        .collect();

    (items.len(), preview)
}

#[derive(Debug, Deserialize)]
struct RemoteHubPluginEnvelope {
    plugins: Vec<RemoteHubPluginWire>,
}

#[derive(Debug, Deserialize)]
struct RemoteHubPluginWire {
    manifest: PluginManifest,
    #[serde(default)]
    skill: Option<SkillDoc>,
    #[serde(default)]
    has_ui_bundle: Option<bool>,
    #[serde(default)]
    icon_url: Option<String>,
    #[serde(default)]
    source: Option<ModuleSource>,
    #[serde(default)]
    runs_in: Option<ModuleRunsIn>,
    #[serde(default)]
    ui_bundle_hash: Option<String>,
    #[serde(default)]
    ui_bundle_size_bytes: Option<u64>,
    #[serde(default)]
    can_install: bool,
    #[serde(default)]
    provider_label: Option<String>,
    #[serde(default)]
    launch_url: Option<String>,
    #[serde(default)]
    marketplace_offer_id: Option<String>,
    #[serde(default)]
    module_attestation: Option<Value>,
}

async fn fetch_hub_plugins(
    http: &Client,
    hub_url: &str,
    auth: Option<&CallerAuth>,
) -> CompassResult<Vec<(RemoteHubModule, Option<Value>)>> {
    let url = format!("{hub_url}/api/v1/plugins");
    let mut req = http
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/json");
    // Only the stored hub session token (`U-2`); this client never holds browser credentials.
    if let Some(bearer) = auth
        .and_then(|caller| caller.authorization.as_deref())
        .filter(|s| !s.is_empty())
    {
        req = req.header("Authorization", bearer);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| CompassError::Storage(format!("fetch hub plugins failed: {}", e.without_url())))?;
    if !resp.status().is_success() {
        return Err(crate::cloud_client::map_http_error(resp).await);
    }
    let body: RemoteHubPluginEnvelope = resp
        .json()
        .await
        .map_err(|e| CompassError::Protocol(format!("decode hub plugins JSON: {e}")))?;

    Ok(body
        .plugins
        .into_iter()
        .map(|dto| {
            let has_ui = dto
                .has_ui_bundle
                .unwrap_or_else(|| dto.ui_bundle_hash.is_some() || dto.manifest.ui.is_some());
            (
                RemoteHubModule {
                    manifest: dto.manifest,
                    skill: dto.skill,
                    source: dto.source.unwrap_or(ModuleSource::OrgCloud),
                    runs_in: dto.runs_in.unwrap_or(ModuleRunsIn::Cloud),
                    has_ui_bundle: has_ui,
                    has_remote_icon: dto.icon_url.is_some(),
                    ui_bundle_hash: dto.ui_bundle_hash,
                    ui_bundle_size_bytes: dto.ui_bundle_size_bytes,
                    can_install: dto.can_install,
                    provider_label: dto.provider_label,
                    launch_url: dto.launch_url,
                    offer_id: dto.marketplace_offer_id,
                    hub_origin: hub_url.to_owned(),
                    ui_attestation: None,
                    ui_fetch_origin: hub_url.to_owned(),
                    ui_attestation_error: None,
                },
                dto.module_attestation,
            )
        })
        .collect())
}

fn synthesize_module_from_offer(
    offer: &MarketplaceOfferSummary,
    hub_origin: &str,
) -> Option<RemoteHubModule> {
    let raw_id = offer
        .plugin_id
        .as_deref()
        .or_else(|| offer.title.contains('/').then_some(offer.title.as_str()))?;
    let plugin_id = PluginId::new(raw_id.to_owned()).ok()?;
    let version = offer
        .version
        .clone()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "1.0.0".to_owned());
    let can_install = offer.kind == "download";
    let provider_label = offer
        .provider_org
        .clone()
        .unwrap_or_else(|| "Network provider".to_owned());
    let has_ui = offer.ui_bundle_hash.is_some();
    let manifest = PluginManifest {
        id: plugin_id,
        name: offer.title.clone(),
        version,
        author: Some(provider_label.clone()),
        description: offer.description.clone().unwrap_or_else(|| {
            if can_install {
                format!("Downloadable network module from {provider_label}.")
            } else {
                format!("Remote network module running at {provider_label}.")
            }
        }),
        category: "Marketplace".to_owned(),
        icon: None,
        execution: if can_install {
            PluginExecutionPlane::Either
        } else {
            PluginExecutionPlane::CloudOnly
        },
        requires_catalog_level: biocompass_core_traits::GrantLevel::default(),
        dataset_input_contract: None,
        runtime_queries: Vec::new(),
        runtime_join_queries: std::collections::BTreeMap::new(),
        child_modules: Default::default(),
        max_child_wait_sec: None,
        max_child_spend_milli_act: None,
        image: format!("network://{hub_origin}/offers/{}", offer.id),
        network: NetworkPolicy::default(),
        price_milli_act: offer.price_milli_act.unwrap_or(0),
        offers: Vec::new(),
        resources: PluginResources::default(),
        inputs: PluginInputs::default(),
        views: Vec::new(),
        ui: has_ui.then(|| PluginUiSpec {
            entry: "ui/dist/index.html".to_owned(),
        }),
        documentation_url: None,
        video_url: None,
        publication_url: None,
    };
    Some(RemoteHubModule {
        manifest,
        skill: None,
        source: ModuleSource::NetworkProvider,
        runs_in: ModuleRunsIn::NetworkProvider,
        has_ui_bundle: has_ui,
        has_remote_icon: false,
        ui_bundle_hash: offer.ui_bundle_hash.clone(),
        ui_bundle_size_bytes: offer.ui_bundle_size_bytes,
        can_install,
        provider_label: Some(provider_label),
        launch_url: offer
            .launch_url
            .clone()
            .or_else(|| Some(format!("{hub_origin}/module-offers/{}", offer.id))),
        offer_id: Some(offer.id.clone()),
        hub_origin: hub_origin.to_owned(),
        ui_attestation: None,
        ui_fetch_origin: hub_origin.to_owned(),
        ui_attestation_error: None,
    })
}

fn synthesize_module_from_subscription(sub: &SubscribedNetworkOffer) -> Option<RemoteHubModule> {
    let offer = MarketplaceOfferSummary {
        id: sub.offer_id.clone(),
        kind: sub.kind.clone(),
        title: sub.title.clone(),
        version: Some(sub.version.clone()),
        provider_org: Some(sub.provider_label.clone()),
        plugin_id: Some(sub.plugin_id.clone()),
        ui_bundle_hash: sub.ui_bundle_hash.clone(),
        ui_bundle_size_bytes: sub.ui_bundle_size_bytes,
        launch_url: sub.launch_url.clone(),
        price_milli_act: Some(sub.price_milli_act),
        description: (!sub.description.is_empty()).then(|| sub.description.clone()),
        signed_offer: None,
    };
    synthesize_module_from_offer(&offer, &sub.hub_url)
}

fn load_persisted_workspace_hub(data_dir: &Path) -> Option<PersistedWorkspaceHub> {
    let path = data_dir.join(WORKSPACE_HUB_FILE_NAME);
    let raw = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<PersistedWorkspaceHub>(&raw) {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "ignoring malformed workspace_hub.json");
            None
        }
    }
}

fn save_persisted_workspace_hub(
    data_dir: &Path,
    hub: &PersistedWorkspaceHub,
) -> CompassResult<()> {
    std::fs::create_dir_all(data_dir)
        .map_err(|e| CompassError::Storage(format!("create data_dir {}: {e}", data_dir.display())))?;
    let encoded = serde_json::to_string_pretty(hub)
        .map_err(|e| CompassError::Internal(format!("encode workspace hub config: {e}")))?;
    let mut tmp = tempfile::NamedTempFile::new_in(data_dir)
        .map_err(|e| CompassError::Storage(format!("create temp workspace hub file: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| CompassError::Storage(format!("chmod temp workspace hub file: {e}")))?;
    }
    tmp.write_all(encoded.as_bytes())
        .map_err(|e| CompassError::Storage(format!("write temp workspace hub file: {e}")))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| CompassError::Storage(format!("sync temp workspace hub file: {e}")))?;
    let dest = data_dir.join(WORKSPACE_HUB_FILE_NAME);
    tmp.persist(&dest)
        .map_err(|e| CompassError::Storage(format!("persist {}: {}", dest.display(), e.error)))?;
    Ok(())
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

    #[test]
    fn b1_parses_live_compass_hub_froglet_v1_feed_artifacts_and_active_hashes() {
        let feed = serde_json::json!({
            "artifacts": [
                {
                    "cursor": 1,
                    "artifact_hash": "hash-desc-1",
                    "payload_hash": "ph-desc-1",
                    "artifact_kind": "descriptor",
                    "actor_id": "provider-alithea",
                    "created_at": 1760000000,
                    "document": {
                        "hash": "hash-desc-1",
                        "signer": "aaaa",
                        "payload": { "provider_id": "provider-alithea" }
                    }
                },
                {
                    "cursor": 2,
                    "artifact_hash": "hash-offer-run",
                    "payload_hash": "ph-run",
                    "artifact_kind": "offer",
                    "actor_id": "provider-alithea",
                    "created_at": 1760000001,
                    "document": {
                        "hash": "hash-offer-run",
                        "signer": "aaaa",
                        "payload": {
                            "provider_id": "provider-alithea",
                            "offer_id": "mo-11111111111111111111111111111111",
                            "offer_kind": "biocompass.module.run.v1",
                            "execution_profile": {
                                "access_handles": ["alithea/hla-motif-enrichment@2.0.0"]
                            }
                        }
                    }
                },
                {
                    "cursor": 3,
                    "artifact_hash": "hash-offer-download",
                    "payload_hash": "ph-dl",
                    "artifact_kind": "offer",
                    "actor_id": "provider-partner",
                    "created_at": 1760000002,
                    "document": {
                        "hash": "hash-offer-download",
                        "signer": "bbbb",
                        "payload": {
                            "provider_id": "provider-partner",
                            "offer_id": "mo-22222222222222222222222222222222",
                            "offer_kind": "biocompass.module.download.v1",
                            "execution_profile": {
                                "access_handles": ["partner/cell-annotator@1.3.0"]
                            }
                        }
                    }
                },
                {
                    "cursor": 4,
                    "artifact_hash": "hash-offer-dataset",
                    "payload_hash": "ph-ds",
                    "artifact_kind": "offer",
                    "actor_id": "provider-alithea",
                    "created_at": 1760000003,
                    "document": {
                        "hash": "hash-offer-dataset",
                        "signer": "aaaa",
                        "payload": {
                            "provider_id": "provider-alithea",
                            "offer_id": "do-33333333333333333333333333333333",
                            "offer_kind": "dataset.ducklake.v1",
                            "execution_profile": {
                                "access_handles": ["alithea/hla-atlas-v2026.1"]
                            }
                        }
                    }
                },
                {
                    "cursor": 5,
                    "artifact_hash": "hash-offer-retired",
                    "payload_hash": "ph-ret",
                    "artifact_kind": "offer",
                    "actor_id": "provider-alithea",
                    "created_at": 1760000004,
                    "document": {
                        "hash": "hash-offer-retired",
                        "signer": "aaaa",
                        "payload": {
                            "provider_id": "provider-alithea",
                            "offer_id": "mo-retired",
                            "offer_kind": "biocompass.module.run.v1",
                            "execution_profile": {
                                "access_handles": ["alithea/old-mod@0.1.0"]
                            }
                        }
                    }
                }
            ],
            "active_offer_hashes": [
                "hash-offer-run",
                "hash-offer-download",
                "hash-offer-dataset"
            ],
            "cursor_type": "artifact_sequence",
            "cursor_semantics": "exclusive_after",
            "applied_cursor": 0,
            "page_size": 50,
            "has_more": false,
            "next_cursor": 5
        });

        let (count, offers) = parse_marketplace_feed(&feed);
        assert_eq!(count, 3, "only active offer artifacts must be counted (excluding descriptor & retired offer)");
        assert_eq!(offers.len(), 3);

        assert_eq!(offers[0].id, "mo-11111111111111111111111111111111");
        assert_eq!(offers[0].kind, "run");
        assert_eq!(offers[0].title, "alithea/hla-motif-enrichment");
        assert_eq!(offers[0].version.as_deref(), Some("2.0.0"));
        assert_eq!(offers[0].provider_org.as_deref(), Some("provider-alithea"));

        assert_eq!(offers[1].id, "mo-22222222222222222222222222222222");
        assert_eq!(offers[1].kind, "download");
        assert_eq!(offers[1].title, "partner/cell-annotator");
        assert_eq!(offers[1].version.as_deref(), Some("1.3.0"));
        assert_eq!(offers[1].provider_org.as_deref(), Some("provider-partner"));

        assert_eq!(offers[2].id, "do-33333333333333333333333333333333");
        assert_eq!(offers[2].kind, "dataset");
        assert_eq!(offers[2].title, "alithea/hla-atlas-v2026.1");
        assert_eq!(offers[2].version, None);
    }

    /// Mock hub that signs real `bc.module_attestation.v1` documents with a swappable key.
    struct MockHub {
        url: String,
        seen_auth: Arc<tokio::sync::Mutex<Vec<String>>>,
        /// Every request's header block (to prove no browser credentials are forwarded).
        requests: Arc<tokio::sync::Mutex<Vec<String>>>,
        key: Arc<std::sync::Mutex<biocompass_node_runtime::froglet_protocol::crypto::NodeSigningKey>>,
        /// When set, the attestation signs a different UI hash than the listing (tamper).
        tamper: Arc<std::sync::atomic::AtomicBool>,
    }

    async fn spawn_mock_hub_server(
        plugin_id: &'static str,
        plugin_name: &'static str,
        ui_bundle_hash: String,
    ) -> (String, Arc<tokio::sync::Mutex<Vec<String>>>) {
        let hub = spawn_signing_mock_hub(plugin_id, plugin_name, ui_bundle_hash).await;
        (hub.url, hub.seen_auth)
    }

    async fn spawn_signing_mock_hub(
        plugin_id: &'static str,
        plugin_name: &'static str,
        ui_bundle_hash: String,
    ) -> MockHub {
        use biocompass_node_runtime::froglet_protocol::crypto::{generate_signing_key, public_key_hex};
        use biocompass_node_runtime::module_attestation::{
            ModuleAttestationPayload, sign_module_attestation,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let seen_auth = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
        let requests = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
        let key = Arc::new(std::sync::Mutex::new(generate_signing_key()));
        let tamper = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (seen_auth_bg, requests_bg, key_bg, tamper_bg) = (
            Arc::clone(&seen_auth),
            Arc::clone(&requests),
            Arc::clone(&key),
            Arc::clone(&tamper),
        );
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                requests_bg.lock().await.push(req.clone());
                for line in req.lines() {
                    if line.to_ascii_lowercase().starts_with("authorization:") {
                        seen_auth_bg.lock().await.push(line.to_owned());
                    }
                }
                let signing_key = key_bg.lock().unwrap().clone();
                let first_line = req.lines().next().unwrap_or("");
                let body = if first_line.starts_with("GET /v1/hub-identity") {
                    serde_json::json!({ "hub_key": public_key_hex(&signing_key) }).to_string()
                } else if first_line.starts_with("GET /api/v1/network-modules") {
                    serde_json::json!({ "modules": [] }).to_string()
                } else if first_line.starts_with("GET /api/v1/plugins") {
                    let attested_hash = if tamper_bg.load(std::sync::atomic::Ordering::SeqCst) {
                        "f".repeat(64)
                    } else {
                        ui_bundle_hash.clone()
                    };
                    let attestation = sign_module_attestation(
                        &signing_key,
                        ModuleAttestationPayload {
                            plugin_id: plugin_id.to_owned(),
                            version: "1.0.0".into(),
                            ui_bundle_hash: Some(attested_hash),
                            ui_bundle_size_bytes: Some(128),
                            image_digest: None,
                            hub_key: String::new(),
                            issued_at: 1_760_000_000,
                            provider: None,
                        },
                    )
                    .unwrap();
                    serde_json::json!({
                        "plugins": [{
                            "manifest": {
                                "id": plugin_id,
                                "name": plugin_name,
                                "version": "1.0.0",
                                "description": format!("Hosted module from {plugin_name}"),
                                "category": "Proteomics",
                                "execution": "cloud_only",
                                "image": "registry.example.org/mod@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                                "price_milli_act": 2500,
                                "ui": { "entry": "ui/dist/index.html" }
                            },
                            "has_ui_bundle": true,
                            "source": "org_cloud",
                            "runs_in": "cloud",
                            "ui_bundle_hash": ui_bundle_hash,
                            "ui_bundle_size_bytes": 128,
                            "can_install": false,
                            "installed_locally": false,
                            "module_attestation": attestation,
                        }]
                    })
                    .to_string()
                } else {
                    serde_json::json!({
                        "artifacts": [{
                            "cursor": 1,
                            "artifact_hash": "feed-offer-1",
                            "artifact_kind": "offer",
                            "actor_id": "provider-hub",
                            "document": {
                                "hash": "feed-offer-1",
                                "payload": {
                                    "provider_id": "provider-hub",
                                    "offer_id": format!("mo-{}", plugin_id.replace('/', "-")),
                                    "offer_kind": "biocompass.module.run.v1",
                                    "execution_profile": {
                                        "access_handles": [format!("{plugin_id}@1.0.0")],
                                        "capabilities": [format!("biocompass.ui_bundle_hash.v1:{ui_bundle_hash}")]
                                    }
                                }
                            }
                        }],
                        "active_offer_hashes": ["feed-offer-1"],
                        "has_more": false
                    })
                    .to_string()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        MockHub {
            url: format!("http://{addr}"),
            seen_auth,
            requests,
            key,
            tamper,
        }
    }

    fn local_cfg(default_hub_url: &str) -> MarketplaceConfig {
        MarketplaceConfig {
            mode: DeploymentMode::Local,
            default_hub_url: default_hub_url.to_owned(),
            explicit_hub_url: None,
            airgapped: false,
        }
    }

    #[tokio::test]
    async fn u1_hub_key_is_pinned_on_first_connection_and_a_changed_key_is_refused_until_retrusted() {
        use biocompass_node_runtime::froglet_protocol::crypto::{generate_signing_key, public_key_hex};
        let hash = "a".repeat(64);
        let hub = spawn_signing_mock_hub("org-a/alpha-mod", "Alpha", hash.clone()).await;
        let tmp = tempfile::tempdir().unwrap();
        let client = MarketplaceClient::new(tmp.path(), local_cfg(&hub.url)).unwrap();
        let status = client
            .set_workspace_hub(WorkspaceHubMode::AlitheaCloud, None, Some("t".into()), None)
            .await
            .unwrap();
        let first_key = public_key_hex(&hub.key.lock().unwrap());
        assert_eq!(status.hub_key.as_deref(), Some(first_key.as_str()));
        assert_eq!(status.hub_key_status, "pinned");
        let mods = client.list_remote_and_subscribed_modules().await;
        assert_eq!(mods[0].ui_bundle_hash.as_deref(), Some(hash.as_str()));
        assert!(mods[0].ui_attestation.is_some());

        // The pin survives a restart.
        let restarted = MarketplaceClient::new(tmp.path(), local_cfg(&hub.url)).unwrap();
        assert_eq!(
            restarted.workspace_hub_status().await.hub_key.as_deref(),
            Some(first_key.as_str())
        );

        // The hub (or an impostor) now signs with another key: refused, nothing openable.
        *hub.key.lock().unwrap() = generate_signing_key();
        let new_key = public_key_hex(&hub.key.lock().unwrap());
        let err = restarted.refresh_connected_hub().await.unwrap_err();
        assert!(err.to_string().contains("UI_HUB_KEY_CHANGED"), "{err}");
        let st = restarted.workspace_hub_status().await;
        assert_eq!(st.hub_key_status, "changed");
        assert_eq!(st.presented_hub_key.as_deref(), Some(new_key.as_str()));
        assert!(restarted.list_remote_and_subscribed_modules().await.is_empty());

        // Explicit re-trust of the new key restores the listing.
        let st = restarted
            .set_workspace_hub(WorkspaceHubMode::AlitheaCloud, None, Some("t".into()), Some(new_key.clone()))
            .await
            .unwrap();
        assert_eq!(st.hub_key.as_deref(), Some(new_key.as_str()));
        assert_eq!(st.hub_key_status, "pinned");
        let mods = restarted.list_remote_and_subscribed_modules().await;
        assert!(mods[0].ui_attestation.is_some());
    }

    #[tokio::test]
    async fn u1_listing_whose_attestation_signs_another_hash_cannot_be_opened() {
        let hash = "a".repeat(64);
        let hub = spawn_signing_mock_hub("org-a/alpha-mod", "Alpha", hash.clone()).await;
        hub.tamper.store(true, std::sync::atomic::Ordering::SeqCst);
        let tmp = tempfile::tempdir().unwrap();
        let client = MarketplaceClient::new(tmp.path(), local_cfg(&hub.url)).unwrap();
        client
            .set_workspace_hub(WorkspaceHubMode::AlitheaCloud, None, Some("t".into()), None)
            .await
            .unwrap();
        let mods = client.list_remote_and_subscribed_modules().await;
        let alpha = mods.iter().find(|m| m.manifest.id.as_str() == "org-a/alpha-mod").unwrap();
        assert!(alpha.ui_attestation.is_none());
        assert!(alpha.ui_bundle_hash.is_none());
        assert!(alpha.ui_attestation_error.as_deref().unwrap_or("").contains("UI_ATTESTATION_INVALID"));
    }

    #[tokio::test]
    async fn u2_module_list_is_cached_for_a_short_ttl_instead_of_refetched_per_call() {
        let hub = spawn_signing_mock_hub("org-a/alpha-mod", "Alpha", "a".repeat(64)).await;
        let tmp = tempfile::tempdir().unwrap();
        let client = MarketplaceClient::new(tmp.path(), local_cfg(&hub.url)).unwrap();
        client
            .set_workspace_hub(WorkspaceHubMode::AlitheaCloud, None, Some("t".into()), None)
            .await
            .unwrap();
        let count = |reqs: &[String]| {
            reqs.iter()
                .filter(|r| r.starts_with("GET /api/v1/plugins"))
                .count()
        };
        let before = count(&hub.requests.lock().await);
        for _ in 0..5 {
            assert_eq!(client.list_remote_and_subscribed_modules().await.len(), 1);
        }
        assert_eq!(count(&hub.requests.lock().await), before, "list calls must hit the TTL cache");
    }

    #[tokio::test]
    async fn subscribe_network_offer_requires_marketplace_consent_and_local_mode() {
        let offer = SubscribedNetworkOffer {
            offer_id: "mo-1".into(),
            hub_url: "https://hub.example.org".into(),
            plugin_id: "org/mod".into(),
            version: "1.0.0".into(),
            title: "Mod".into(),
            description: String::new(),
            kind: "run".into(),
            provider_label: "Provider".into(),
            ui_bundle_hash: None,
            ui_bundle_size_bytes: None,
            launch_url: None,
            price_milli_act: 0,
        };
        let tmp = tempfile::tempdir().unwrap();
        let unconsented = MarketplaceClient::new(tmp.path(), local_cfg("https://hub.example.org")).unwrap();
        let err = unconsented.subscribe_network_offer(offer.clone()).await.unwrap_err();
        assert!(err.to_string().contains("MARKETPLACE_CONSENT_REQUIRED"), "{err}");

        let tmp2 = tempfile::tempdir().unwrap();
        let cloud = MarketplaceClient::new(
            tmp2.path(),
            MarketplaceConfig {
                mode: DeploymentMode::CloudWeb,
                default_hub_url: "https://hub.example.org".into(),
                explicit_hub_url: None,
                airgapped: false,
            },
        )
        .unwrap();
        let err = cloud.subscribe_network_offer(offer).await.unwrap_err();
        assert!(err.to_string().contains("cloud-web"), "{err}");
    }

    #[tokio::test]
    async fn u3_local_client_switches_between_two_hubs_in_app_persists_and_isolates_auth() {
        let hash_a = "a".repeat(64);
        let hash_b = "b".repeat(64);
        let (hub_a, auth_a) =
            spawn_mock_hub_server("org-a/alpha-mod", "Alpha Cloud Module", hash_a.clone()).await;
        let (hub_b, auth_b) =
            spawn_mock_hub_server("org-b/beta-mod", "Beta Customer Module", hash_b.clone()).await;
        let tmp = tempfile::tempdir().expect("tempdir");

        let cfg = MarketplaceConfig {
            mode: DeploymentMode::Local,
            default_hub_url: hub_a.clone(),
            explicit_hub_url: None,
            airgapped: false,
        };
        let client = MarketplaceClient::new(tmp.path(), cfg.clone()).expect("client");

        // 1. Switch to Alithea Cloud (Hub A) with a bearer token.
        let status_a = client
            .set_workspace_hub(
                WorkspaceHubMode::AlitheaCloud,
                None,
                Some("secret-token-hub-a".to_owned()),
                None,
            )
            .await
            .expect("switch to hub A");
        assert_eq!(status_a.mode, WorkspaceHubMode::AlitheaCloud);
        assert_eq!(status_a.hub_url.as_deref(), Some(hub_a.as_str()));
        assert!(status_a.connected);
        assert!(status_a.authenticated);
        let mods_a = client.list_remote_and_subscribed_modules().await;
        assert_eq!(mods_a.len(), 1);
        assert_eq!(mods_a[0].manifest.id.as_str(), "org-a/alpha-mod");
        assert_eq!(mods_a[0].ui_bundle_hash.as_deref(), Some(hash_a.as_str()));
        assert!(!auth_a.lock().await.is_empty());

        // 2. Switch to Custom Cloud (Hub B) — token from Hub A MUST NOT leak to Hub B!
        let status_b = client
            .set_workspace_hub(WorkspaceHubMode::CustomCloud, Some(hub_b.clone()), None, None)
            .await
            .expect("switch to hub B");
        assert_eq!(status_b.mode, WorkspaceHubMode::CustomCloud);
        assert_eq!(status_b.hub_url.as_deref(), Some(hub_b.as_str()));
        assert!(!status_b.authenticated, "Hub A token must be cleared on hub switch");
        let mods_b = client.list_remote_and_subscribed_modules().await;
        assert_eq!(mods_b.len(), 1);
        assert_eq!(mods_b[0].manifest.id.as_str(), "org-b/beta-mod");
        assert_eq!(mods_b[0].ui_bundle_hash.as_deref(), Some(hash_b.as_str()));
        assert!(
            auth_b.lock().await.is_empty(),
            "Hub A auth header leaked to Hub B!"
        );

        // 3. Restart on same data_dir preserves Hub B choice without needing COMPASS_HUB_URL env!
        let restarted = MarketplaceClient::new(tmp.path(), cfg.clone()).expect("restart");
        let persisted_status = restarted.workspace_hub_status().await;
        assert_eq!(persisted_status.mode, WorkspaceHubMode::CustomCloud);
        assert_eq!(persisted_status.hub_url.as_deref(), Some(hub_b.as_str()));
        assert!(persisted_status.recent_hubs.contains(&hub_a));
        assert!(persisted_status.recent_hubs.contains(&hub_b));

        // 4. Switch to Offline clears remote modules cleanly.
        let offline = restarted
            .set_workspace_hub(WorkspaceHubMode::Offline, None, None, None)
            .await
            .expect("offline");
        assert_eq!(offline.mode, WorkspaceHubMode::Offline);
        assert_eq!(offline.hub_url, None);
        assert!(restarted.list_remote_and_subscribed_modules().await.is_empty());
    }
}
