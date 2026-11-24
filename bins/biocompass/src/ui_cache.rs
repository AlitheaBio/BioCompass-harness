//! Verified content-addressed UI bundle cache (`U1`) with pre-compressed `gzip` serving,
//! single-auth-per-open viewer-bound tickets, disk persistence in `local` mode, and bounded
//! icon caching.

use crate::cloud_client::CallerAuth;
use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use biocompass_core_traits::{CompassError, CompassResult, ErrorCode};
use biocompass_plugin_spec::MAX_UI_BUNDLE_BYTES;
use chrono::Utc;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, RwLock, Semaphore};
use uuid::Uuid;

/// Maximum number of files permitted inside one UI bundle (matches `hub-registry/src/ui_bundle.rs`).
pub const MAX_UI_BUNDLE_FILES: usize = 128;

/// Maximum encoded HTTP archive body size (base64 JSON overhead over 32 MiB raw cap).
const MAX_ARCHIVE_HTTP_BYTES: usize = (MAX_UI_BUNDLE_BYTES as usize) * 2 + 1024 * 1024;

/// Default memory budget for cached decompressed + pre-compressed UI bundles (128 MiB).
const MAX_MEMORY_CACHE_BYTES: u64 = 128 * 1024 * 1024;

/// Single-open viewer-bound UI ticket lifetime (5 minutes, aligned with `ui_release.rs`).
pub const OPEN_TICKET_LIFETIME_SECS: i64 = 300;

/// Maximum concurrent open tickets retained in memory.
const MAX_OPEN_TICKETS: usize = 4096;

/// Icon cache entry TTL and max entry count.
const ICON_CACHE_TTL_SECS: u64 = 300;
const MAX_ICON_CACHE_ENTRIES: usize = 256;
const MAX_ICON_BYTES: usize = 256 * 1024;
const MAX_MODULE_SOURCES: usize = 2048;

/// Prefix distinguishing workspace-issued U1 cache tickets from legacy hub tickets.
pub const U1_TICKET_PREFIX: &str = "u1-";

/// Canonical file inventory: sorted relative path -> `(sha256_hex, size_bytes)`.
pub type UiFileInventory = BTreeMap<String, (String, u64)>;

/// Single pre-compressed asset stored inside a verified UI bundle.
#[derive(Debug, Clone)]
pub struct CachedUiAsset {
    pub mime: String,
    pub sha256: String,
    pub raw: Bytes,
    pub gzipped: Bytes,
}

impl CachedUiAsset {
    fn from_raw(path: &str, raw: Vec<u8>) -> Self {
        let sha256 = sha256_hex(&raw);
        let mime = mime_guess::from_path(path)
            .first_or_octet_stream()
            .essence_str()
            .to_owned();
        let gzipped = Bytes::from(gzip_compress(&raw));
        Self {
            mime,
            sha256,
            raw: Bytes::from(raw),
            gzipped,
        }
    }

    fn memory_bytes(&self) -> u64 {
        (self.raw.len() + self.gzipped.len()) as u64
    }
}

/// Immutable, SHA-256 verified UI bundle ready for sub-millisecond serving.
#[derive(Debug)]
pub struct CachedUiBundle {
    pub ui_bundle_hash: String,
    pub entry: String,
    pub total_raw_bytes: u64,
    pub files: BTreeMap<String, CachedUiAsset>,
    pub cached_at: Instant,
}

impl CachedUiBundle {
    fn memory_bytes(&self) -> u64 {
        self.files.values().map(CachedUiAsset::memory_bytes).sum()
    }
}

/// Origin pointer recorded when listing/subscribing/syncing a remote or network module.
///
/// Intentionally stores NO caller credentials (`Cookie` / `Authorization` / `X-API-Key`);
/// credentials are always supplied ephemerally by the active request or local hub session.
#[derive(Debug, Clone)]
pub struct RemoteUiSource {
    pub hub_origin: String,
    pub ui_bundle_hash: String,
    pub entry: String,
}

/// Short-lived viewer-bound authorization grant minted ONCE per module open (`U1` + `SEC-17`).
#[derive(Debug, Clone)]
pub struct UiOpenGrant {
    pub org_id: Option<String>,
    pub session_sha256: Option<String>,
    pub plugin_id: String,
    pub version: String,
    pub ui_bundle_hash: Option<String>,
    pub local_ui_root: Option<PathBuf>,
    pub entry: String,
    pub expires_at: i64,
}

/// Cached plugin icon bytes + content-type (`serve_plugin_icon` cache).
#[derive(Debug, Clone)]
pub struct CachedIcon {
    pub content_type: String,
    pub bytes: Bytes,
    pub cached_at: Instant,
}

/// JSON wire schema for `GET /v1/artifacts/<ui_bundle_hash>` (and compatible variants).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiBundleArchiveJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_bundle_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    pub files: Vec<UiBundleArchiveFileJson>,
}

/// Single file entry inside a JSON UI bundle archive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiBundleArchiveFileJson {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_b64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_hex: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DiskBundleManifest {
    format: u8,
    ui_bundle_hash: String,
    entry: String,
    total_raw_bytes: u64,
    files: UiFileInventory,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ModuleUiKey {
    scope: String,
    plugin_id: String,
    version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IconCacheKey {
    scope: String,
    ns: String,
    slug: String,
}

/// Workspace-level verified UI bundle cache (`local`: disk + memory, `cloud-web`: memory + optional disk).
pub struct UiBundleCache {
    disk_dir: Option<PathBuf>,
    http: Client,
    memory: RwLock<HashMap<String, Arc<CachedUiBundle>>>,
    memory_bytes: AtomicU64,
    in_flight: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    prefetch_slots: Arc<Semaphore>,
    module_sources: RwLock<HashMap<ModuleUiKey, RemoteUiSource>>,
    local_dir_cache: RwLock<HashMap<PathBuf, (u64, Arc<CachedUiBundle>)>>,
    tickets: RwLock<HashMap<String, UiOpenGrant>>,
    icons: RwLock<HashMap<IconCacheKey, CachedIcon>>,
}

impl UiBundleCache {
    /// Construct a new UI bundle cache. When `persist_to_disk` is true (`local` mode),
    /// verified bundles are stored under `<data_dir>/ui-cache/<hash>/`.
    pub fn new(data_dir: &Path, persist_to_disk: bool) -> Self {
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent(format!("biocompass-ui-cache/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("build UI cache HTTP client");
        let disk_dir = persist_to_disk.then(|| data_dir.join("ui-cache"));
        Self {
            disk_dir,
            http,
            memory: RwLock::new(HashMap::new()),
            memory_bytes: AtomicU64::new(0),
            in_flight: Mutex::new(HashMap::new()),
            prefetch_slots: Arc::new(Semaphore::new(4)),
            module_sources: RwLock::new(HashMap::new()),
            local_dir_cache: RwLock::new(HashMap::new()),
            tickets: RwLock::new(HashMap::new()),
            icons: RwLock::new(HashMap::new()),
        }
    }

    /// Return the disk cache root directory, if enabled.
    pub fn disk_dir(&self) -> Option<&Path> {
        self.disk_dir.as_deref()
    }

    /// Record a module's signed UI bundle hash and origin hub for fast open & pre-fetch,
    /// isolated by `scope` (`"local"` or hashed tenant session/credentials in `cloud-web`).
    pub async fn register_remote_ui(
        &self,
        scope: &str,
        plugin_id: &str,
        version: &str,
        source: RemoteUiSource,
    ) {
        if !is_sha256_hex(&source.ui_bundle_hash) {
            return;
        }
        let key = ModuleUiKey {
            scope: scope.to_owned(),
            plugin_id: plugin_id.to_owned(),
            version: version.to_owned(),
        };
        let mut sources = self.module_sources.write().await;
        if sources.len() >= MAX_MODULE_SOURCES && !sources.contains_key(&key) {
            sources.clear();
        }
        sources.insert(key, source);
    }

    /// Clear all registered remote UI pointers belonging to `scope` (e.g. on local hub switch).
    pub async fn clear_remote_ui_scope(&self, scope: &str) {
        let mut sources = self.module_sources.write().await;
        sources.retain(|k, _| k.scope != scope);
    }

    /// Look up the registered UI bundle metadata for `(scope, plugin_id, version)`.
    pub async fn lookup_remote_ui(
        &self,
        scope: &str,
        plugin_id: &str,
        version: &str,
    ) -> Option<RemoteUiSource> {
        let key = ModuleUiKey {
            scope: scope.to_owned(),
            plugin_id: plugin_id.to_owned(),
            version: version.to_owned(),
        };
        self.module_sources.read().await.get(&key).cloned()
    }

    /// Check memory (then disk, verifying SHA-256 on disk load) without hitting the network.
    pub async fn get_cached(&self, hash: &str) -> CompassResult<Option<Arc<CachedUiBundle>>> {
        validate_hash(hash)?;
        if let Some(bundle) = self.memory.read().await.get(hash).cloned() {
            return Ok(Some(bundle));
        }
        if let Some(root) = &self.disk_dir
            && let Some(bundle) = load_verified_from_disk(root, hash)?
        {
            let bundle = Arc::new(bundle);
            self.insert_memory(Arc::clone(&bundle)).await;
            return Ok(Some(bundle));
        }
        Ok(None)
    }

    /// Fetch (cold path: single GET `/v1/artifacts/<hash>`), verify SHA-256 (refusing tampered
    /// bundles with [`ErrorCode::UiBundleTampered`]), store pre-compressed, and return the bundle.
    pub async fn get_or_fetch(
        &self,
        hash: &str,
        hub_origin: &str,
        auth: Option<&CallerAuth>,
    ) -> CompassResult<Arc<CachedUiBundle>> {
        validate_hash(hash)?;
        if let Some(hit) = self.get_cached(hash).await? {
            return Ok(hit);
        }

        // Single-flight deduplication so concurrent opens fetch the bundle at most once.
        let flight_lock = {
            let mut map = self.in_flight.lock().await;
            Arc::clone(
                map.entry(hash.to_owned())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        let _guard = flight_lock.lock().await;

        // Re-check cache after winning the single-flight lock.
        if let Some(hit) = self.get_cached(hash).await? {
            return Ok(hit);
        }

        let clean_origin = hub_origin.trim().trim_end_matches('/');
        let url = format!("{clean_origin}/v1/artifacts/{hash}");
        let mut req = self
            .http
            .get(&url)
            .header(reqwest::header::ACCEPT, "application/json, application/x-tar");
        if let Some(caller) = auth {
            if let Some(cookie) = caller.cookie.as_deref().filter(|s| !s.is_empty()) {
                req = req.header("Cookie", cookie);
            }
            if let Some(bearer) = caller.authorization.as_deref().filter(|s| !s.is_empty()) {
                req = req.header("Authorization", bearer);
            }
            if let Some(key) = caller.api_key.as_deref().filter(|s| !s.is_empty()) {
                req = req.header("X-API-Key", key);
            }
        }

        let resp = req.send().await.map_err(|e| {
            CompassError::Storage(format!("fetch UI bundle archive failed: {}", e.without_url()))
        })?;
        if !resp.status().is_success() {
            return Err(crate::cloud_client::map_http_error(resp).await);
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if resp
            .content_length()
            .is_some_and(|len| len as usize > MAX_ARCHIVE_HTTP_BYTES)
        {
            return Err(CompassError::Validation(format!(
                "UI bundle archive exceeds {} byte cap",
                MAX_UI_BUNDLE_BYTES
            )));
        }

        let mut body = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                CompassError::Storage(format!("read UI bundle stream failed: {}", e.without_url()))
            })?;
            if body.len().saturating_add(chunk.len()) > MAX_ARCHIVE_HTTP_BYTES {
                return Err(CompassError::Validation(format!(
                    "UI bundle archive exceeds {} byte cap",
                    MAX_UI_BUNDLE_BYTES
                )));
            }
            body.extend_from_slice(&chunk);
        }

        let bundle = parse_and_verify_ui_archive(hash, content_type.as_deref(), &body)?;
        if let Some(root) = &self.disk_dir {
            persist_bundle_to_disk(root, &bundle)?;
        }
        let bundle = Arc::new(bundle);
        self.insert_memory(Arc::clone(&bundle)).await;

        // Clean up the single-flight lock entry if uncontended.
        drop(_guard);
        let mut map = self.in_flight.lock().await;
        if let Some(entry) = map.get(hash)
            && Arc::strong_count(entry) == 1
        {
            map.remove(hash);
        }

        Ok(bundle)
    }

    /// Asynchronously warm the UI cache in the background when modules are listed/subscribed/synced.
    pub fn prefetch_background(
        self: &Arc<Self>,
        hash: String,
        hub_origin: String,
        auth: Option<CallerAuth>,
    ) {
        if !is_sha256_hex(&hash) || hub_origin.trim().is_empty() {
            return;
        }
        let cache = Arc::clone(self);
        tokio::spawn(async move {
            let Ok(_permit) = cache.prefetch_slots.clone().acquire_owned().await else {
                return;
            };
            if let Ok(Some(_)) = cache.get_cached(&hash).await {
                return;
            }
            if let Err(err) = cache.get_or_fetch(&hash, &hub_origin, auth.as_ref()).await {
                tracing::debug!(ui_bundle_hash = %hash, error = %err, "background UI bundle prefetch skipped");
            }
        });
    }

    /// Load & cache a local plugin's on-disk `ui/dist` directory with pre-compressed gzip assets.
    pub async fn load_local_dir(
        &self,
        ui_dist_dir: &Path,
        entry_name: &str,
    ) -> CompassResult<Arc<CachedUiBundle>> {
        let fingerprint = compute_dir_fingerprint(ui_dist_dir)?;
        if let Some((cached_fp, bundle)) = self.local_dir_cache.read().await.get(ui_dist_dir)
            && *cached_fp == fingerprint
        {
            return Ok(Arc::clone(bundle));
        }

        let mut raw_files = BTreeMap::new();
        collect_local_ui_files(ui_dist_dir, ui_dist_dir, &mut raw_files)?;
        let (ui_bundle_hash, _inventory, total_raw_bytes) = compute_ui_inventory_hash(&raw_files)?;
        let entry = choose_entry_file(&raw_files, Some(entry_name))?;
        let files = raw_files
            .into_iter()
            .map(|(path, raw)| {
                let asset = CachedUiAsset::from_raw(&path, raw);
                (path, asset)
            })
            .collect();
        let bundle = Arc::new(CachedUiBundle {
            ui_bundle_hash,
            entry,
            total_raw_bytes,
            files,
            cached_at: Instant::now(),
        });
        let mut cache = self.local_dir_cache.write().await;
        if cache.len() >= 128 {
            cache.clear();
        }
        cache.insert(ui_dist_dir.to_path_buf(), (fingerprint, Arc::clone(&bundle)));
        Ok(bundle)
    }

    /// Issue a short-lived viewer-bound U1 open ticket (`u1-<64-hex>`) after single-open auth.
    pub async fn issue_open_ticket(&self, grant: UiOpenGrant) -> String {
        let now = Utc::now().timestamp();
        let raw_a = Uuid::new_v4();
        let raw_b = Uuid::new_v4();
        let mut seed = [0u8; 32];
        seed[..16].copy_from_slice(raw_a.as_bytes());
        seed[16..].copy_from_slice(raw_b.as_bytes());
        let ticket = format!("{U1_TICKET_PREFIX}{}", hex::encode(seed));

        let mut map = self.tickets.write().await;
        if map.len() >= MAX_OPEN_TICKETS {
            map.retain(|_, v| v.expires_at > now);
            if map.len() >= MAX_OPEN_TICKETS {
                map.clear();
            }
        }
        map.insert(ticket.clone(), grant);
        ticket
    }

    /// Validate a U1 open ticket, preserving `SEC-17` viewer-session binding on entry loads.
    pub async fn verify_open_ticket(
        &self,
        ticket: &str,
        headers: &HeaderMap,
        asset_path: &str,
    ) -> CompassResult<UiOpenGrant> {
        let now = Utc::now().timestamp();
        let grant = {
            let map = self.tickets.read().await;
            map.get(ticket).cloned()
        }
        .ok_or_else(|| CompassError::AccessDenied("invalid or expired module UI access".into()))?;

        if grant.expires_at <= now {
            self.tickets.write().await.remove(ticket);
            return Err(CompassError::AccessDenied(
                "invalid or expired module UI access".into(),
            ));
        }

        let presented_session = extract_browser_session_sha256(headers);
        let is_entry = is_entry_document_request(headers, asset_path);
        if let Some(expected_session) = grant.session_sha256.as_deref()
            && (is_entry || presented_session.is_some())
        {
            let matches = presented_session
                .as_deref()
                .is_some_and(|got| constant_time_eq_str(got, expected_session));
            if !matches {
                return Err(CompassError::AccessDenied(
                    "FORBIDDEN: plugin UI access ticket is bound to a different session".into(),
                ));
            }
        }

        Ok(grant)
    }

    /// Look up a cached icon by `(scope, ns, slug)`.
    pub async fn get_cached_icon(&self, scope: &str, ns: &str, slug: &str) -> Option<CachedIcon> {
        let key = IconCacheKey {
            scope: scope.to_owned(),
            ns: ns.to_owned(),
            slug: slug.to_owned(),
        };
        let guard = self.icons.read().await;
        guard
            .get(&key)
            .filter(|entry| entry.cached_at.elapsed().as_secs() < ICON_CACHE_TTL_SECS)
            .cloned()
    }

    /// Store a validated icon in the bounded icon cache.
    pub async fn put_cached_icon(&self, scope: &str, ns: &str, slug: &str, icon: CachedIcon) {
        let key = IconCacheKey {
            scope: scope.to_owned(),
            ns: ns.to_owned(),
            slug: slug.to_owned(),
        };
        let mut guard = self.icons.write().await;
        if guard.len() >= MAX_ICON_CACHE_ENTRIES {
            guard.retain(|_, v| v.cached_at.elapsed().as_secs() < ICON_CACHE_TTL_SECS);
            if guard.len() >= MAX_ICON_CACHE_ENTRIES {
                guard.clear();
            }
        }
        guard.insert(key, icon);
    }

    /// Fetch a plugin icon from an upstream hub with a hard `256 KiB` streamed body cap
    /// (preventing unbounded upstream responses from consuming workspace memory).
    pub async fn fetch_bounded_icon(
        &self,
        url: &str,
        auth: Option<&CallerAuth>,
    ) -> CompassResult<Option<(String, Bytes)>> {
        let mut req = self.http.get(url);
        if let Some(caller) = auth {
            if let Some(cookie) = caller.cookie.as_deref().filter(|s| !s.is_empty()) {
                req = req.header("Cookie", cookie);
            }
            if let Some(bearer) = caller.authorization.as_deref().filter(|s| !s.is_empty()) {
                req = req.header("Authorization", bearer);
            }
            if let Some(key) = caller.api_key.as_deref().filter(|s| !s.is_empty()) {
                req = req.header("X-API-Key", key);
            }
        }
        let Ok(resp) = req.send().await else {
            return Ok(None);
        };
        if !resp.status().is_success() {
            return Ok(None);
        }
        if resp
            .content_length()
            .is_some_and(|len| len as usize > MAX_ICON_BYTES)
        {
            return Err(CompassError::Validation(format!(
                "plugin icon exceeds {MAX_ICON_BYTES} byte limit"
            )));
        }
        let ct = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        let mut buf = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                CompassError::Storage(format!("read icon stream failed: {}", e.without_url()))
            })?;
            if buf.len().saturating_add(chunk.len()) > MAX_ICON_BYTES {
                return Err(CompassError::Validation(format!(
                    "plugin icon exceeds {MAX_ICON_BYTES} byte limit"
                )));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(Some((ct, Bytes::from(buf))))
    }

    async fn insert_memory(&self, bundle: Arc<CachedUiBundle>) {
        let added = bundle.memory_bytes();
        let mut map = self.memory.write().await;
        if let Some(prev) = map.get(&bundle.ui_bundle_hash) {
            let prev_bytes = prev.memory_bytes();
            self.memory_bytes.fetch_sub(prev_bytes, Ordering::Relaxed);
        }
        let current = self.memory_bytes.load(Ordering::Relaxed);
        if current.saturating_add(added) > MAX_MEMORY_CACHE_BYTES && !map.is_empty() {
            // Evict oldest cached entries until under budget.
            let mut order: Vec<(String, Instant, u64)> = map
                .iter()
                .map(|(k, v)| (k.clone(), v.cached_at, v.memory_bytes()))
                .collect();
            order.sort_by_key(|(_, at, _)| *at);
            let mut total = current;
            for (old_hash, _, bytes) in order {
                if total.saturating_add(added) <= MAX_MEMORY_CACHE_BYTES {
                    break;
                }
                if map.remove(&old_hash).is_some() {
                    total = total.saturating_sub(bytes);
                }
            }
            self.memory_bytes.store(total, Ordering::Relaxed);
        }
        map.insert(bundle.ui_bundle_hash.clone(), bundle);
        self.memory_bytes.fetch_add(added, Ordering::Relaxed);
    }
}

/// Render an asset from a verified `CachedUiBundle` with pre-compressed `Content-Encoding: gzip`
/// whenever the caller advertises `Accept-Encoding: gzip`.
pub fn render_cached_ui_asset(
    bundle: &CachedUiBundle,
    asset_path: &str,
    headers: &HeaderMap,
) -> CompassResult<Response> {
    let requested = asset_path.trim_start_matches('/');
    let rel = if requested.is_empty() {
        bundle.entry.as_str()
    } else {
        requested
    };
    if !is_safe_relative_path(rel) {
        return Err(CompassError::AccessDenied(
            "unsafe path traversal in plugin UI asset".into(),
        ));
    }
    let asset = bundle
        .files
        .get(rel)
        .ok_or_else(|| CompassError::NotFound(format!("plugin UI asset '{rel}' not found")))?;

    let wants_gzip = client_accepts_gzip(headers);
    let mut resp = if wants_gzip {
        (StatusCode::OK, asset.gzipped.clone()).into_response()
    } else {
        (StatusCode::OK, asset.raw.clone()).into_response()
    };
    if let Ok(ct) = HeaderValue::from_str(&asset.mime) {
        resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    if wants_gzip {
        resp.headers_mut()
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        resp.headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    }
    if let Ok(etag) = HeaderValue::from_str(&format!("\"{}\"", asset.sha256)) {
        resp.headers_mut().insert(header::ETAG, etag);
    }
    Ok(resp)
}

/// Compute a deterministic SHA-256 inventory hash identical to `hub-registry/src/ui_bundle.rs`.
pub fn compute_ui_inventory_hash(
    files: &BTreeMap<String, Vec<u8>>,
) -> CompassResult<(String, UiFileInventory, u64)> {
    if files.is_empty() || files.len() > MAX_UI_BUNDLE_FILES {
        return Err(CompassError::Validation(format!(
            "UI bundle must contain 1..={MAX_UI_BUNDLE_FILES} files"
        )));
    }
    let mut total_bytes = 0u64;
    let mut inventory = UiFileInventory::new();
    for (path, bytes) in files {
        if !is_safe_relative_path(path) {
            return Err(CompassError::ui_bundle_tampered(format!(
                "invalid relative UI bundle path '{path}'"
            )));
        }
        let len = bytes.len() as u64;
        total_bytes = total_bytes
            .checked_add(len)
            .ok_or_else(|| CompassError::Validation("UI bundle size overflow".into()))?;
        if total_bytes > MAX_UI_BUNDLE_BYTES {
            return Err(CompassError::Validation(format!(
                "UI bundle exceeds {MAX_UI_BUNDLE_BYTES} bytes"
            )));
        }
        inventory.insert(path.clone(), (sha256_hex(bytes), len));
    }
    for path in inventory.keys() {
        for (idx, _) in path.match_indices('/') {
            if inventory.contains_key(&path[..idx]) {
                return Err(CompassError::ui_bundle_tampered(format!(
                    "UI bundle file/directory collision at '{path}'"
                )));
            }
        }
    }
    let canonical = serde_json::to_vec(&inventory)
        .map_err(|e| CompassError::Internal(format!("serialize UI inventory: {e}")))?;
    Ok((sha256_hex(&canonical), inventory, total_bytes))
}

/// Parse an archive fetched from `/v1/artifacts/<hash>` (JSON `ui_bundle.v1` or POSIX ustar TAR)
/// and cryptographically verify it against `expected_hash`.
///
/// Any hash mismatch, per-file checksum mismatch, or archive corruption fails closed with
/// [`ErrorCode::UiBundleTampered`].
pub fn parse_and_verify_ui_archive(
    expected_hash: &str,
    content_type: Option<&str>,
    body: &[u8],
) -> CompassResult<CachedUiBundle> {
    validate_hash(expected_hash)?;
    if body.is_empty() {
        return Err(CompassError::ui_bundle_tampered(
            "UI bundle archive response was empty",
        ));
    }

    let is_tar = content_type
        .is_some_and(|ct| ct.to_ascii_lowercase().contains("tar"))
        || (body.len() >= 512 && &body[257..262] == b"ustar");

    let (raw_files, declared_entry, declared_hash) = if is_tar {
        (parse_ustar_archive(body)?, None, None)
    } else {
        let parsed = parse_json_archive(body)?;
        (parsed.files, parsed.entry, parsed.ui_bundle_hash)
    };

    if let Some(decl) = declared_hash.as_deref()
        && !constant_time_eq_str(decl, expected_hash)
    {
        return Err(CompassError::ui_bundle_tampered(format!(
            "declared UI bundle hash '{decl}' does not match expected '{expected_hash}'"
        )));
    }

    let (inventory_hash, _inventory, total_raw_bytes) = compute_ui_inventory_hash(&raw_files)?;
    let raw_archive_hash = sha256_hex(body);
    let verified = constant_time_eq_str(&inventory_hash, expected_hash)
        || (declared_hash.is_none() && constant_time_eq_str(&raw_archive_hash, expected_hash));
    if !verified {
        return Err(CompassError::ui_bundle_tampered(format!(
            "UI bundle digest mismatch: expected '{expected_hash}', computed inventory '{inventory_hash}'"
        )));
    }

    let entry = choose_entry_file(&raw_files, declared_entry.as_deref())?;
    let files = raw_files
        .into_iter()
        .map(|(path, raw)| {
            let asset = CachedUiAsset::from_raw(&path, raw);
            (path, asset)
        })
        .collect();

    Ok(CachedUiBundle {
        ui_bundle_hash: expected_hash.to_owned(),
        entry,
        total_raw_bytes,
        files,
        cached_at: Instant::now(),
    })
}

/// Build a canonical JSON `ui_bundle.v1` archive payload from `(path -> bytes)` and optional entry.
pub fn encode_ui_bundle_archive_json(
    files: &BTreeMap<String, Vec<u8>>,
    entry: Option<&str>,
) -> CompassResult<(String, Vec<u8>)> {
    let (ui_bundle_hash, inventory, _) = compute_ui_inventory_hash(files)?;
    let resolved_entry = choose_entry_file(files, entry)?;
    let encoded_files = files
        .iter()
        .map(|(path, bytes)| {
            let (file_sha, size_bytes) = &inventory[path];
            UiBundleArchiveFileJson {
                path: path.clone(),
                sha256: Some(file_sha.clone()),
                size_bytes: Some(*size_bytes),
                data_b64: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
                data_hex: None,
            }
        })
        .collect();
    let payload = UiBundleArchiveJson {
        kind: Some("ui_bundle.v1".to_owned()),
        ui_bundle_hash: Some(ui_bundle_hash.clone()),
        entry: Some(resolved_entry),
        files: encoded_files,
    };
    let bytes = serde_json::to_vec(&payload)
        .map_err(|e| CompassError::Internal(format!("encode UI bundle archive: {e}")))?;
    Ok((ui_bundle_hash, bytes))
}

struct ParsedJsonArchive {
    files: BTreeMap<String, Vec<u8>>,
    entry: Option<String>,
    ui_bundle_hash: Option<String>,
}

fn parse_json_archive(body: &[u8]) -> CompassResult<ParsedJsonArchive> {
    let archive: UiBundleArchiveJson = serde_json::from_slice(body).map_err(|e| {
        CompassError::ui_bundle_tampered(format!("invalid UI bundle JSON archive: {e}"))
    })?;
    if archive.files.is_empty() || archive.files.len() > MAX_UI_BUNDLE_FILES {
        return Err(CompassError::ui_bundle_tampered(format!(
            "UI bundle archive must contain 1..={MAX_UI_BUNDLE_FILES} files"
        )));
    }
    let mut out = BTreeMap::new();
    let mut total_bytes = 0u64;
    for entry in archive.files {
        if !is_safe_relative_path(&entry.path) || out.contains_key(&entry.path) {
            return Err(CompassError::ui_bundle_tampered(format!(
                "invalid or duplicate UI bundle path '{}'",
                entry.path
            )));
        }
        let bytes = if let Some(b64) = entry.data_b64.as_deref() {
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| {
                    CompassError::ui_bundle_tampered(format!(
                        "invalid base64 payload for '{}': {e}",
                        entry.path
                    ))
                })?
        } else if let Some(hx) = entry.data_hex.as_deref() {
            hex::decode(hx).map_err(|e| {
                CompassError::ui_bundle_tampered(format!(
                    "invalid hex payload for '{}': {e}",
                    entry.path
                ))
            })?
        } else {
            return Err(CompassError::ui_bundle_tampered(format!(
                "UI bundle file '{}' missing content encoding",
                entry.path
            )));
        };
        let len = bytes.len() as u64;
        if let Some(expected_len) = entry.size_bytes
            && expected_len != len
        {
            return Err(CompassError::ui_bundle_tampered(format!(
                "UI bundle file '{}' size mismatch (declared {expected_len}, actual {len})",
                entry.path
            )));
        }
        if let Some(expected_sha) = entry.sha256.as_deref() {
            let actual_sha = sha256_hex(&bytes);
            if !constant_time_eq_str(&actual_sha, expected_sha) {
                return Err(CompassError::ui_bundle_tampered(format!(
                    "UI bundle file '{}' checksum mismatch",
                    entry.path
                )));
            }
        }
        total_bytes = total_bytes.saturating_add(len);
        if total_bytes > MAX_UI_BUNDLE_BYTES {
            return Err(CompassError::Validation(format!(
                "UI bundle exceeds {MAX_UI_BUNDLE_BYTES} bytes"
            )));
        }
        out.insert(entry.path, bytes);
    }
    Ok(ParsedJsonArchive {
        files: out,
        entry: archive.entry,
        ui_bundle_hash: archive.ui_bundle_hash,
    })
}

fn parse_ustar_archive(body: &[u8]) -> CompassResult<BTreeMap<String, Vec<u8>>> {
    let mut out = BTreeMap::new();
    let mut offset = 0usize;
    let mut total_bytes = 0u64;
    while offset + 512 <= body.len() {
        let header = &body[offset..offset + 512];
        offset += 512;
        if header.iter().all(|&b| b == 0) {
            break;
        }
        // Validate header checksum per POSIX ustar.
        let stored_sum = parse_octal(&header[148..156]).ok_or_else(|| {
            CompassError::ui_bundle_tampered("invalid tar header checksum field")
        })?;
        let computed_sum: u64 = header
            .iter()
            .enumerate()
            .map(|(i, &b)| if (148..156).contains(&i) { b' ' as u64 } else { b as u64 })
            .sum();
        if stored_sum != computed_sum {
            return Err(CompassError::ui_bundle_tampered(
                "corrupted tar header checksum in UI bundle",
            ));
        }

        let typeflag = header[156];
        let size = parse_octal(&header[124..136])
            .ok_or_else(|| CompassError::ui_bundle_tampered("invalid tar entry size"))?;
        let name_part = cstr_slice(&header[0..100]);
        let prefix_part = cstr_slice(&header[345..500]);
        let raw_path = if prefix_part.is_empty() {
            name_part.to_owned()
        } else {
            format!("{prefix_part}/{name_part}")
        };
        let path = raw_path.trim_start_matches("./").to_owned();

        let padded_len = ((size as usize).checked_add(511).ok_or_else(|| {
            CompassError::ui_bundle_tampered("tar entry size overflow")
        })? / 512)
            * 512;
        if offset.saturating_add(padded_len) > body.len() {
            return Err(CompassError::ui_bundle_tampered(
                "truncated tar entry in UI bundle",
            ));
        }
        let data = &body[offset..offset + size as usize];
        offset += padded_len;

        match typeflag {
            b'5' => continue, // directory entry
            b'0' | b'\0' => {
                if !is_safe_relative_path(&path) || out.contains_key(&path) {
                    return Err(CompassError::ui_bundle_tampered(format!(
                        "invalid or duplicate tar UI path '{path}'"
                    )));
                }
                total_bytes = total_bytes.saturating_add(size);
                if total_bytes > MAX_UI_BUNDLE_BYTES || out.len() >= MAX_UI_BUNDLE_FILES {
                    return Err(CompassError::Validation(format!(
                        "UI bundle exceeds size or file-count limit ({MAX_UI_BUNDLE_BYTES} bytes, {MAX_UI_BUNDLE_FILES} files)"
                    )));
                }
                out.insert(path, data.to_vec());
            }
            _ => {
                return Err(CompassError::ui_bundle_tampered(format!(
                    "disallowed non-regular tar entry type '{}' in UI bundle",
                    typeflag as char
                )));
            }
        }
    }
    if out.is_empty() {
        return Err(CompassError::ui_bundle_tampered(
            "UI bundle tar archive contained no files",
        ));
    }
    Ok(out)
}

fn cstr_slice(raw: &[u8]) -> &str {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    std::str::from_utf8(&raw[..end]).unwrap_or("").trim()
}

fn parse_octal(raw: &[u8]) -> Option<u64> {
    let text = cstr_slice(raw);
    if text.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(text, 8).ok()
}

fn persist_bundle_to_disk(cache_root: &Path, bundle: &CachedUiBundle) -> CompassResult<()> {
    let target_dir = cache_root.join(&bundle.ui_bundle_hash);
    if target_dir.join("manifest.json").is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(cache_root)
        .map_err(|e| CompassError::Storage(format!("create ui-cache root: {e}")))?;

    let tmp_dir = cache_root.join(format!(
        ".tmp-{}-{}",
        &bundle.ui_bundle_hash[..12],
        Uuid::new_v4().simple()
    ));
    let files_dir = tmp_dir.join("files");
    std::fs::create_dir_all(&files_dir)
        .map_err(|e| CompassError::Storage(format!("create ui-cache temp dir: {e}")))?;

    let mut inventory = UiFileInventory::new();
    for (rel_path, asset) in &bundle.files {
        inventory.insert(rel_path.clone(), (asset.sha256.clone(), asset.raw.len() as u64));
        let key = sha256_hex(rel_path.as_bytes());
        // Persist ONLY verified raw bytes on disk; gzipped representation is always derived
        // in memory from SHA-256-verified raw bytes on reload (never trusted from unverified `.gz`).
        write_file_atomic(&files_dir.join(format!("{key}.raw")), &asset.raw)?;
    }
    let manifest = DiskBundleManifest {
        format: 1,
        ui_bundle_hash: bundle.ui_bundle_hash.clone(),
        entry: bundle.entry.clone(),
        total_raw_bytes: bundle.total_raw_bytes,
        files: inventory,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| CompassError::Internal(format!("encode ui-cache manifest: {e}")))?;
    write_file_atomic(&tmp_dir.join("manifest.json"), &manifest_bytes)?;

    if let Err(err) = std::fs::rename(&tmp_dir, &target_dir) {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        if !target_dir.join("manifest.json").is_file() {
            return Err(CompassError::Storage(format!(
                "commit ui-cache bundle directory: {err}"
            )));
        }
    }
    Ok(())
}

fn load_verified_from_disk(cache_root: &Path, expected_hash: &str) -> CompassResult<Option<CachedUiBundle>> {
    let dir = cache_root.join(expected_hash);
    let manifest_path = dir.join("manifest.json");
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|e| CompassError::Storage(format!("read ui-cache manifest: {e}")))?;
    let manifest: DiskBundleManifest = serde_json::from_slice(&manifest_bytes).map_err(|e| {
        CompassError::ui_bundle_tampered(format!("corrupted ui-cache manifest on disk: {e}"))
    })?;
    if manifest.format != 1 || !constant_time_eq_str(&manifest.ui_bundle_hash, expected_hash) {
        return Err(CompassError::ui_bundle_tampered(
            "cached UI bundle manifest hash mismatch on disk",
        ));
    }

    let files_dir = dir.join("files");
    let mut raw_files = BTreeMap::new();
    let mut cached_assets = BTreeMap::new();
    for (rel_path, (expected_sha, expected_size)) in &manifest.files {
        if !is_safe_relative_path(rel_path) {
            return Err(CompassError::ui_bundle_tampered(format!(
                "unsafe cached UI path '{rel_path}'"
            )));
        }
        let key = sha256_hex(rel_path.as_bytes());
        let raw_path = files_dir.join(format!("{key}.raw"));
        let raw = std::fs::read(&raw_path).map_err(|e| {
            CompassError::ui_bundle_tampered(format!("missing cached UI asset '{rel_path}': {e}"))
        })?;
        if raw.len() as u64 != *expected_size || !constant_time_eq_str(&sha256_hex(&raw), expected_sha) {
            return Err(CompassError::ui_bundle_tampered(format!(
                "cached UI asset '{rel_path}' failed integrity verification"
            )));
        }
        // Always derive pre-compressed gzip bytes directly from SHA-256-verified raw bytes
        // so an attacker modifying a `.gz` sidecar on disk can never inject tampered content.
        let gzipped = gzip_compress(&raw);
        let mime = mime_guess::from_path(rel_path)
            .first_or_octet_stream()
            .essence_str()
            .to_owned();
        raw_files.insert(rel_path.clone(), raw.clone());
        cached_assets.insert(
            rel_path.clone(),
            CachedUiAsset {
                mime,
                sha256: expected_sha.clone(),
                raw: Bytes::from(raw),
                gzipped: Bytes::from(gzipped),
            },
        );
    }

    let (recomputed_hash, _, total_raw_bytes) = compute_ui_inventory_hash(&raw_files)?;
    if !constant_time_eq_str(&recomputed_hash, expected_hash) {
        return Err(CompassError::ui_bundle_tampered(format!(
            "cached UI inventory hash mismatch: expected '{expected_hash}', got '{recomputed_hash}'"
        )));
    }
    let entry = choose_entry_file(&raw_files, Some(&manifest.entry))?;
    Ok(Some(CachedUiBundle {
        ui_bundle_hash: expected_hash.to_owned(),
        entry,
        total_raw_bytes,
        files: cached_assets,
        cached_at: Instant::now(),
    }))
}

fn write_file_atomic(path: &Path, bytes: &[u8]) -> CompassResult<()> {
    let mut file = std::fs::File::create(path)
        .map_err(|e| CompassError::Storage(format!("create ui-cache file: {e}")))?;
    file.write_all(bytes)
        .map_err(|e| CompassError::Storage(format!("write ui-cache file: {e}")))?;
    Ok(())
}

fn collect_local_ui_files(
    root: &Path,
    dir: &Path,
    out: &mut BTreeMap<String, Vec<u8>>,
) -> CompassResult<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| CompassError::Storage(format!("read local UI dir: {e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| CompassError::Storage(format!("read local UI entry: {e}")))?;
        let path = entry.path();
        let meta = entry
            .metadata()
            .map_err(|e| CompassError::Storage(format!("stat local UI entry: {e}")))?;
        if meta.is_dir() {
            collect_local_ui_files(root, &path, out)?;
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| CompassError::Internal(format!("relativize UI path: {e}")))?;
            let rel_str = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if !is_safe_relative_path(&rel_str) {
                continue;
            }
            let bytes = std::fs::read(&path)
                .map_err(|e| CompassError::Storage(format!("read local UI file '{rel_str}': {e}")))?;
            out.insert(rel_str, bytes);
        }
    }
    Ok(())
}

fn compute_dir_fingerprint(dir: &Path) -> CompassResult<u64> {
    let mut acc = 0xcbf2_9ce4_8422_2325u64;
    fn walk(dir: &Path, acc: &mut u64) -> CompassResult<()> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| CompassError::Storage(format!("fingerprint local UI dir: {e}")))?
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let meta = entry
                .metadata()
                .map_err(|e| CompassError::Storage(format!("stat local UI entry: {e}")))?;
            for b in entry.file_name().to_string_lossy().as_bytes() {
                *acc = acc.wrapping_mul(0x0100_0000_01b3) ^ (*b as u64);
            }
            if meta.is_dir() {
                walk(&path, acc)?;
            } else if meta.is_file() {
                *acc = acc.wrapping_mul(0x0100_0000_01b3) ^ meta.len();
                if let Ok(mtime) = meta.modified()
                    && let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH)
                {
                    *acc = acc.wrapping_mul(0x0100_0000_01b3) ^ dur.as_nanos() as u64;
                }
            }
        }
        Ok(())
    }
    walk(dir, &mut acc)?;
    Ok(acc)
}

fn choose_entry_file(
    files: &BTreeMap<String, Vec<u8>>,
    declared: Option<&str>,
) -> CompassResult<String> {
    if let Some(raw) = declared.map(str::trim).filter(|s| !s.is_empty()) {
        let basename = Path::new(raw)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(raw);
        if files.contains_key(raw) {
            return Ok(raw.to_owned());
        }
        if files.contains_key(basename) {
            return Ok(basename.to_owned());
        }
    }
    if files.contains_key("index.html") {
        return Ok("index.html".to_owned());
    }
    if let Some(first_html) = files.keys().find(|p| p.ends_with(".html")) {
        return Ok(first_html.clone());
    }
    files
        .keys()
        .next()
        .cloned()
        .ok_or_else(|| CompassError::coded(ErrorCode::UiBundleTampered, "UI bundle has no files"))
}

pub fn is_safe_relative_path(raw: &str) -> bool {
    !raw.is_empty()
        && raw.trim() == raw
        && !raw.contains('\\')
        && !raw.chars().any(char::is_control)
        && !raw.split('/').any(|part| matches!(part, "" | "." | ".."))
        && Path::new(raw)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

pub fn is_sha256_hex(raw: &str) -> bool {
    raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn validate_hash(raw: &str) -> CompassResult<()> {
    if !is_sha256_hex(raw) {
        return Err(CompassError::Validation(format!(
            "invalid UI bundle sha256 digest '{raw}'"
        )));
    }
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn constant_time_eq_str(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

pub fn extract_browser_session_sha256(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in cookie.split(';') {
        if let Some(val) = part.trim().strip_prefix("bc_session=") {
            let token = val.trim();
            if !token.is_empty() {
                return Some(sha256_hex(token.as_bytes()));
            }
        }
    }
    None
}

/// Compute a tenant-safe cache scope key from caller credentials so private org icons and
/// remote UI source pointers never leak across sessions or organizations.
pub fn icon_cache_scope_for_headers(headers: &HeaderMap) -> String {
    let mut hasher = Sha256::new();
    let mut has_any = false;
    for name in [
        header::COOKIE.as_str(),
        header::AUTHORIZATION.as_str(),
        "x-api-key",
    ] {
        if let Some(val) = headers.get(name) {
            hasher.update(name.as_bytes());
            hasher.update(b"=");
            hasher.update(val.as_bytes());
            hasher.update(b"\n");
            has_any = true;
        }
    }
    if has_any {
        hex::encode(hasher.finalize())
    } else {
        "anonymous".to_owned()
    }
}

/// Compute the same tenant-safe cache scope key from an active [`CallerAuth`] context.
pub fn caller_auth_scope(auth: &CallerAuth) -> String {
    let mut hasher = Sha256::new();
    let mut has_any = false;
    for (name, val) in [
        (header::COOKIE.as_str(), auth.cookie.as_deref()),
        (header::AUTHORIZATION.as_str(), auth.authorization.as_deref()),
        ("x-api-key", auth.api_key.as_deref()),
    ] {
        if let Some(val) = val.filter(|s| !s.is_empty()) {
            hasher.update(name.as_bytes());
            hasher.update(b"=");
            hasher.update(val.as_bytes());
            hasher.update(b"\n");
            has_any = true;
        }
    }
    if has_any {
        hex::encode(hasher.finalize())
    } else {
        "anonymous".to_owned()
    }
}

fn is_entry_document_request(headers: &HeaderMap, asset: &str) -> bool {
    let dest_is_frame = headers
        .get("sec-fetch-dest")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|d| {
            let trimmed = d.trim();
            trimmed.eq_ignore_ascii_case("iframe")
                || trimmed.eq_ignore_ascii_case("frame")
                || trimmed.eq_ignore_ascii_case("embed")
                || trimmed.eq_ignore_ascii_case("object")
        });
    let clean = asset.trim().trim_start_matches('/').to_ascii_lowercase();
    let html_or_svg = clean.is_empty()
        || clean.ends_with(".html")
        || clean.ends_with(".htm")
        || clean.ends_with(".xhtml")
        || clean.ends_with(".svg");
    dest_is_frame || html_or_svg
}

fn client_accepts_gzip(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|val| {
            val.split(',').any(|item| {
                let mut parts = item.trim().split(';');
                let enc = parts.next().unwrap_or("").trim();
                if !enc.eq_ignore_ascii_case("gzip") && enc != "*" {
                    return false;
                }
                // Respect explicit `q=0` refusal.
                !parts.any(|p| {
                    p.trim()
                        .strip_prefix("q=")
                        .is_some_and(|q| q.trim() == "0" || q.trim() == "0.0" || q.trim() == "0.000")
                })
            })
        })
}

// ---------------------------------------------------------------------------
// Pure-Rust RFC 1952 Gzip + RFC 1951 Fixed-Huffman LZ77 Deflate (`unsafe_code = "forbid"`)
// ---------------------------------------------------------------------------

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

struct BitWriter {
    buf: Vec<u8>,
    bit_buf: u64,
    bit_count: u8,
}

impl BitWriter {
    fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
            bit_buf: 0,
            bit_count: 0,
        }
    }

    fn write_bits_lsb(&mut self, value: u32, count: u8) {
        self.bit_buf |= (value as u64) << self.bit_count;
        self.bit_count += count;
        while self.bit_count >= 8 {
            self.buf.push((self.bit_buf & 0xff) as u8);
            self.bit_buf >>= 8;
            self.bit_count -= 8;
        }
    }

    fn write_huffman_msb(&mut self, code: u16, count: u8) {
        let mut reversed = 0u32;
        for i in 0..count {
            if (code >> (count - 1 - i)) & 1 != 0 {
                reversed |= 1 << i;
            }
        }
        self.write_bits_lsb(reversed, count);
    }

    fn write_fixed_litlen(&mut self, sym: u16) {
        match sym {
            0..=143 => self.write_huffman_msb(0x30 + sym, 8),
            144..=255 => self.write_huffman_msb(0x190 + (sym - 144), 9),
            256..=279 => self.write_huffman_msb(sym - 256, 7),
            280..=287 => self.write_huffman_msb(0xc0 + (sym - 280), 8),
            _ => unreachable!("invalid deflate symbol"),
        }
    }

    fn write_fixed_dist(&mut self, code: u16) {
        self.write_huffman_msb(code, 5);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bit_count > 0 {
            self.buf.push((self.bit_buf & 0xff) as u8);
        }
        self.buf
    }
}

fn crc32_ieee(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn deflate_fixed_lz77(input: &[u8]) -> Vec<u8> {
    const HASH_BITS: usize = 15;
    const HASH_SIZE: usize = 1 << HASH_BITS;
    const WINDOW: usize = 32_768;
    const MAX_MATCH: usize = 258;

    let mut writer = BitWriter::with_capacity(input.len() / 2 + 16);
    // BFINAL=1, BTYPE=01 (fixed Huffman)
    writer.write_bits_lsb(0b011, 3);

    if input.len() < 3 {
        for &b in input {
            writer.write_fixed_litlen(b as u16);
        }
        writer.write_fixed_litlen(256);
        return writer.finish();
    }

    let mut head = vec![u32::MAX; HASH_SIZE];
    let mut prev = vec![u32::MAX; WINDOW];

    let hash3 = |pos: usize| -> usize {
        let v = ((input[pos] as usize) << 10)
            ^ ((input[pos + 1] as usize) << 5)
            ^ (input[pos + 2] as usize);
        v & (HASH_SIZE - 1)
    };

    let mut pos = 0usize;
    while pos < input.len() {
        let mut best_len = 0usize;
        let mut best_dist = 0usize;

        if pos + 2 < input.len() {
            let h = hash3(pos);
            let mut candidate = head[h];
            prev[pos & (WINDOW - 1)] = candidate;
            head[h] = pos as u32;

            let max_len = MAX_MATCH.min(input.len() - pos);
            let mut chain_steps = 0usize;
            while candidate != u32::MAX && chain_steps < 32 {
                let c = candidate as usize;
                if pos <= c || pos - c > WINDOW {
                    break;
                }
                if input[c] == input[pos]
                    && input[c + 1] == input[pos + 1]
                    && input[c + 2] == input[pos + 2]
                {
                    let mut len = 3usize;
                    while len < max_len && input[c + len] == input[pos + len] {
                        len += 1;
                    }
                    if len > best_len {
                        best_len = len;
                        best_dist = pos - c;
                        if best_len == max_len {
                            break;
                        }
                    }
                }
                let next = prev[c & (WINDOW - 1)];
                if next >= candidate {
                    break;
                }
                candidate = next;
                chain_steps += 1;
            }
        }

        if best_len >= 3 {
            let len_idx = LENGTH_BASE
                .iter()
                .rposition(|&b| (b as usize) <= best_len)
                .unwrap_or(0);
            writer.write_fixed_litlen(257 + len_idx as u16);
            let extra_len_bits = LENGTH_EXTRA[len_idx];
            if extra_len_bits > 0 {
                writer.write_bits_lsb((best_len - LENGTH_BASE[len_idx] as usize) as u32, extra_len_bits);
            }

            let dist_idx = DIST_BASE
                .iter()
                .rposition(|&b| (b as usize) <= best_dist)
                .unwrap_or(0);
            writer.write_fixed_dist(dist_idx as u16);
            let extra_dist_bits = DIST_EXTRA[dist_idx];
            if extra_dist_bits > 0 {
                writer.write_bits_lsb((best_dist - DIST_BASE[dist_idx] as usize) as u32, extra_dist_bits);
            }

            // Index intermediate positions covered by the match.
            for step in 1..best_len {
                let p = pos + step;
                if p + 2 < input.len() {
                    let h = hash3(p);
                    prev[p & (WINDOW - 1)] = head[h];
                    head[h] = p as u32;
                }
            }
            pos += best_len;
        } else {
            writer.write_fixed_litlen(input[pos] as u16);
            pos += 1;
        }
    }

    writer.write_fixed_litlen(256);
    writer.finish()
}

fn deflate_stored(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() + (input.len() / 65535 + 1) * 5 + 5);
    if input.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
        return out;
    }
    let chunks = input.chunks(65_535);
    let total = chunks.len();
    for (idx, chunk) in chunks.enumerate() {
        let is_final = u8::from(idx + 1 == total);
        out.push(is_final);
        let len = chunk.len() as u16;
        let nlen = !len;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&nlen.to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out
}

/// Compress `input` into a standards-compliant RFC 1952 `gzip` stream.
pub fn gzip_compress(input: &[u8]) -> Vec<u8> {
    let deflated = deflate_fixed_lz77(input);
    let payload = if deflated.len() <= input.len() + 5 {
        deflated
    } else {
        deflate_stored(input)
    };
    let mut out = Vec::with_capacity(10 + payload.len() + 8);
    // RFC 1952 header: ID1=1f ID2=8b CM=08 FLG=00 MTIME=0 XFL=00 OS=ff
    out.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xff]);
    out.extend_from_slice(&payload);
    out.extend_from_slice(&crc32_ieee(input).to_le_bytes());
    out.extend_from_slice(&(input.len() as u32).to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn gunzip_with_system_gzip(compressed: &[u8]) -> Vec<u8> {
        let mut child = Command::new("gzip")
            .arg("-dc")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn gzip -dc");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(compressed)
            .expect("write compressed bytes");
        let output = child.wait_with_output().expect("wait gzip");
        assert!(
            output.status.success(),
            "gzip -dc failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    #[test]
    fn gzip_compress_round_trips_and_shrinks_ui_assets() {
        let samples: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"a".to_vec(),
            b"<!doctype html><html><body>Hello BioCompass!</body></html>".to_vec(),
            "export function render(){return '<div>BioCompass UI bundle repeated text</div>';}\n"
                .repeat(400)
                .into_bytes(),
            // Pseudo-random incompressible payload > 64 KiB (exercises stored-block fallback too)
            (0..70_000u32)
                .map(|i| ((i.wrapping_mul(2_654_435_761)) >> 24) as u8)
                .collect(),
        ];

        for (idx, sample) in samples.iter().enumerate() {
            let gz = gzip_compress(sample);
            assert_eq!(&gz[..2], &[0x1f, 0x8b], "sample {idx} missing gzip magic");
            let decoded = gunzip_with_system_gzip(&gz);
            assert_eq!(&decoded, sample, "sample {idx} failed gzip round-trip");
            if idx == 3 {
                assert!(
                    gz.len() * 4 < sample.len(),
                    "expected significant compression on JS bundle (raw={} gz={})",
                    sample.len(),
                    gz.len()
                );
            }
        }
    }

    #[test]
    fn archive_verification_accepts_valid_and_refuses_tampered_with_error_code() {
        let mut files = BTreeMap::new();
        files.insert(
            "index.html".to_owned(),
            b"<!doctype html><script src=\"./app.js\"></script>".to_vec(),
        );
        files.insert("app.js".to_owned(), b"console.log('verified');".to_vec());

        let (expected_hash, archive_bytes) =
            encode_ui_bundle_archive_json(&files, Some("index.html")).unwrap();
        let verified =
            parse_and_verify_ui_archive(&expected_hash, Some("application/json"), &archive_bytes)
                .unwrap();
        assert_eq!(verified.ui_bundle_hash, expected_hash);
        assert_eq!(verified.entry, "index.html");
        assert_eq!(verified.files.len(), 2);

        // Tamper 1 byte inside app.js payload -> MUST fail with ErrorCode::UiBundleTampered!
        let mut tampered_json: UiBundleArchiveJson =
            serde_json::from_slice(&archive_bytes).unwrap();
        tampered_json.files[0].data_b64 = Some(
            base64::engine::general_purpose::STANDARD.encode(b"console.log('TAMPERED');"),
        );
        let tampered_bytes = serde_json::to_vec(&tampered_json).unwrap();
        let err = parse_and_verify_ui_archive(
            &expected_hash,
            Some("application/json"),
            &tampered_bytes,
        )
        .unwrap_err();
        assert!(
            err.is_code(ErrorCode::UiBundleTampered),
            "expected UI_BUNDLE_TAMPERED, got {err:?}"
        );

        // Tamper hash expectation with completely valid archive -> MUST also fail with UiBundleTampered!
        let wrong_hash = "0".repeat(64);
        let err2 = parse_and_verify_ui_archive(
            &wrong_hash,
            Some("application/json"),
            &archive_bytes,
        )
        .unwrap_err();
        assert!(err2.is_code(ErrorCode::UiBundleTampered));
    }

    #[tokio::test]
    async fn disk_cache_persists_verifies_and_refuses_on_disk_tamper() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = BTreeMap::new();
        files.insert("index.html".to_owned(), b"<h1>Hello</h1>".to_vec());
        files.insert("assets/main.js".to_owned(), b"window.READY=1;".to_vec());
        let (hash, archive_bytes) =
            encode_ui_bundle_archive_json(&files, Some("index.html")).unwrap();
        let bundle =
            parse_and_verify_ui_archive(&hash, Some("application/json"), &archive_bytes).unwrap();

        let cache_root = dir.path().join("ui-cache");
        persist_bundle_to_disk(&cache_root, &bundle).unwrap();

        // Fresh UiBundleCache instance reloads & verifies from disk cleanly.
        let fresh = UiBundleCache::new(dir.path(), true);
        let loaded = fresh.get_cached(&hash).await.unwrap().expect("disk hit");
        assert_eq!(loaded.files.len(), 2);

        // Plant a malicious `.gz` file on disk while `.raw` is untouched -> reload MUST ignore
        // the unverified `.gz` and serve gzip bytes derived strictly from the verified `.raw` bytes!
        let key = sha256_hex(b"assets/main.js");
        let evil_gz = gzip_compress(b"window.EVIL=1;");
        std::fs::write(
            cache_root.join(&hash).join("files").join(format!("{key}.gz")),
            &evil_gz,
        )
        .unwrap();
        let fresh_gz = UiBundleCache::new(dir.path(), true);
        let loaded_gz = fresh_gz.get_cached(&hash).await.unwrap().expect("disk hit");
        let asset_gz = loaded_gz.files.get("assets/main.js").unwrap();
        assert_eq!(asset_gz.raw.as_ref(), b"window.READY=1;");
        assert_eq!(
            asset_gz.gzipped.as_ref(),
            gzip_compress(b"window.READY=1;").as_slice(),
            "gzipped payload must be derived from SHA-256-verified raw bytes, never an unverified .gz sidecar"
        );
        assert_ne!(asset_gz.gzipped.as_ref(), evil_gz.as_slice());

        // Tamper a cached `.raw` file on disk -> reload MUST refuse with UiBundleTampered!
        std::fs::write(
            cache_root.join(&hash).join("files").join(format!("{key}.raw")),
            b"window.EVIL=1;",
        )
        .unwrap();
        let fresh2 = UiBundleCache::new(dir.path(), true);
        let err = fresh2.get_cached(&hash).await.unwrap_err();
        assert!(err.is_code(ErrorCode::UiBundleTampered));
    }
}
