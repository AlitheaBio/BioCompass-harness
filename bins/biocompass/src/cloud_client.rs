//! Stateless `cloud-web` trait adapters delegating sessions, S3 blobs, ACT ledger, catalogs,
//! and WSS `froglet-node` container execution to `compass-hub` over standard HTTP/SSE.
//!
//! Enforces Milestone 0 per-request caller authentication (`B9`), scoped blob access (`B1`),
//! and hub-authoritative run state (`B6`).

pub(crate) mod agent_budget;
#[cfg(test)]
mod quote_admission_tests;

use crate::state::WorkspaceRegistryPort;
use async_trait::async_trait;
use axum::http::{HeaderMap, HeaderValue, StatusCode as AxumStatusCode, header};
use axum::response::{IntoResponse, Response};
use biocompass_agent_core::tools::AgentExecutionPort;
use biocompass_core_traits::{
    BlobStore, CatalogId, CompassError, CompassResult, CreditAmountAct, DatasetGrant, LedgerTx,
    NodeId, OrgId, PaymentGateway, PluginId, PrepareRunRequest, ProjectId, ProjectRecord, QueryAst,
    RunDispatcher, RunId, RunLogEvent, RunQuote, RunRecord, SessionEvent, SessionEventKind,
    SessionId, SessionRecord, SessionStore,
};
use biocompass_data_engine::{CatalogSummary, QueryTableResult};
use biocompass_plugin_spec::{LoadedPlugin, PluginManifest, SkillDoc};
use futures::StreamExt;
use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::{RwLock, Semaphore, SemaphorePermit, broadcast};

/// Per-request caller credentials extracted from incoming browser/CLI requests in `cloud-web`.
#[derive(Debug, Clone, Default)]
pub struct CallerAuth {
    pub cookie: Option<String>,
    pub authorization: Option<String>,
    pub api_key: Option<String>,
}

tokio::task_local! {
    pub static CALLER_AUTH: CallerAuth;
}

/// Snapshot the task-local `CallerAuth` (returns empty credentials outside an HTTP request scope).
pub fn current_caller_auth() -> CallerAuth {
    CALLER_AUTH.try_with(Clone::clone).unwrap_or_default()
}

fn apply_caller_auth_from(
    mut req: reqwest::RequestBuilder,
    auth: &CallerAuth,
) -> reqwest::RequestBuilder {
    if let Some(cookie) = auth.cookie.as_deref().filter(|s| !s.is_empty()) {
        req = req.header("Cookie", cookie);
    }
    if let Some(bearer) = auth.authorization.as_deref().filter(|s| !s.is_empty()) {
        req = req.header("Authorization", bearer);
    }
    if let Some(key) = auth.api_key.as_deref().filter(|s| !s.is_empty()) {
        req = req.header("X-API-Key", key);
    }
    req
}

pub(crate) fn apply_workspace_auth(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    let mut auth = current_caller_auth();
    auth.cookie = auth.cookie.as_deref().and_then(|cookies| {
        cookies
            .split(';')
            .map(str::trim)
            .find(|part| part.starts_with("bc_session="))
            .map(str::to_owned)
    });
    apply_caller_auth_from(req, &auth)
}

fn apply_caller_auth(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    let auth = current_caller_auth();
    apply_caller_auth_from(req, &auth)
}

#[derive(Debug, Clone)]
pub struct CompassHubConfig {
    pub base_url: String,
    pub bundled_plugins_dir: PathBuf,
}

impl CompassHubConfig {
    pub fn from_env(bundled_plugins_dir: PathBuf) -> CompassResult<Self> {
        let base_url = std::env::var("COMPASS_HUB_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8081".into())
            .trim_end_matches('/')
            .to_owned();
        Ok(Self {
            base_url,
            bundled_plugins_dir,
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct HubHealth {
    pub node_id: NodeId,
    pub provider_id: String,
    pub requester_id: String,
    pub container_engine: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct HubPluginDto {
    manifest: PluginManifest,
    skill: Option<SkillDoc>,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    has_ui_bundle: Option<bool>,
}

pub struct CompassHubClient {
    http: Client,
    cfg: CompassHubConfig,
    prepare_slots: Semaphore,
    node_id: NodeId,
    provider_id: String,
    requester_id: String,
    local_ui_roots: RwLock<HashMap<PluginId, PathBuf>>,
    log_tx: broadcast::Sender<RunLogEvent>,
}

impl CompassHubClient {
    pub(crate) fn try_prepare_slot(&self) -> CompassResult<SemaphorePermit<'_>> {
        self.prepare_slots.try_acquire().map_err(|_| CompassError::RateLimited(
            "Too many quotes are being prepared. Retry shortly.".into(),
        ))
    }

    pub async fn connect(
        cfg: CompassHubConfig,
        bundled_ui_roots: HashMap<PluginId, PathBuf>,
    ) -> CompassResult<(Self, OrgId, String)> {
        let prepare_limit = match std::env::var("BC_WORKBENCH_MAX_CONCURRENT_PREPARES") {
            Ok(value) => value.parse::<usize>().ok()
                .filter(|limit| (1..=Semaphore::MAX_PERMITS).contains(limit))
                .ok_or_else(|| CompassError::Validation(
                    "BC_WORKBENCH_MAX_CONCURRENT_PREPARES must be a positive supported integer".into(),
                ))?,
            Err(std::env::VarError::NotPresent) => 16,
            Err(_) => return Err(CompassError::Validation(
                "BC_WORKBENCH_MAX_CONCURRENT_PREPARES must be a positive supported integer".into(),
            )),
        };
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| CompassError::Internal(format!("build hub HTTP client: {e}")))?;

        // Startup probe uses only the unauthenticated /api/v1/health endpoint (B9: zero shared API key).
        let health_resp = http
            .get(format!("{}/api/v1/health", cfg.base_url))
            .send()
            .await
            .map_err(|e| CompassError::Internal(format!("GET /api/v1/health failed: {e}")))?;
        if !health_resp.status().is_success() {
            return Err(map_http_error(health_resp).await);
        }
        let health: HubHealth = health_resp
            .json()
            .await
            .map_err(|e| CompassError::Internal(format!("decode GET /api/v1/health JSON: {e}")))?;
        let (log_tx, _) = broadcast::channel(1_024);

        let engine_label = health.container_engine.clone();
        let placeholder_org = OrgId::new("org-cloud-session").map_err(CompassError::Validation)?;
        let client = Self {
            http,
            cfg,
            prepare_slots: Semaphore::new(prepare_limit),
            node_id: health.node_id,
            provider_id: health.provider_id,
            requester_id: health.requester_id,
            local_ui_roots: RwLock::new(bundled_ui_roots),
            log_tx,
        };
        Ok((client, placeholder_org, engine_label))
    }

    pub async fn fetch_grants(&self) -> CompassResult<Vec<DatasetGrant>> {
        #[derive(Deserialize)]
        struct Resp {
            grants: Vec<DatasetGrant>,
        }
        let r: Resp = get_json(&self.http, &self.cfg, "/api/v1/grants").await?;
        Ok(r.grants)
    }

    pub async fn offered_run_module(&self, offer_id: &str) -> CompassResult<(LoadedPlugin, Option<bool>)> {
        let valid = offer_id.strip_prefix("mo-").is_some_and(|suffix| {
            suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
        if !valid {
            return Err(CompassError::Validation("invalid module offer ID".into()));
        }
        let dto: HubPluginDto = get_json(
            &self.http, &self.cfg, &format!("/api/v1/module-offers/{offer_id}/run-module"),
        ).await?;
        Ok((LoadedPlugin { manifest: dto.manifest, skill: dto.skill, root_dir: None }, dto.has_ui_bundle))
    }

    pub async fn list_plugins_with_ui(&self) -> CompassResult<Vec<(LoadedPlugin, Option<bool>)>> {
        #[derive(Deserialize)]
        struct Resp {
            plugins: Vec<HubPluginDto>,
        }
        let r: Resp = get_json(&self.http, &self.cfg, "/api/v1/plugins").await?;
        let roots = self.local_ui_roots.read().await;
        Ok(r.plugins.into_iter().map(|dto| {
            let root_dir = roots.get(&dto.manifest.id).cloned();
            (LoadedPlugin { manifest: dto.manifest, skill: dto.skill, root_dir }, dto.has_ui_bundle)
        }).collect())
    }

    /// Transparently forward an HTTP request to `compass-hub`, preserving caller credentials,
    /// status codes, `Location`, `Content-Type`, and `Set-Cookie` headers.
    pub async fn proxy_raw(
        &self,
        method: Method,
        path_and_query: &str,
        incoming_headers: &HeaderMap,
        body: Vec<u8>,
    ) -> CompassResult<Response> {
        let clean = if path_and_query.starts_with('/') {
            path_and_query.to_owned()
        } else {
            format!("/{path_and_query}")
        };
        let safe_path = if clean.starts_with("/plugin-ui-access/") { "/plugin-ui-access/[redacted]" } else { &clean };
        let url = format!("{}{}", self.cfg.base_url, clean);
        let path = clean.split('?').next().unwrap_or(&clean);
        let is_shared_file = path.starts_with("/api/v1/result-shares/") && path.ends_with("/download");
        let is_prepare = method == Method::POST && path == "/api/v1/runs/prepare";
        let mut req = self.http.request(method, &url);
        if is_shared_file && let Some(range) = incoming_headers.get(header::RANGE) {
            req = req.header(header::RANGE, range);
        }

        for name in [
            "cookie",
            "authorization",
            "x-api-key",
            "sec-fetch-dest",
            "content-type",
            "accept",
            "x-mollie-signature",
        ] {
            if let Some(val) = incoming_headers.get(name)
                && let Ok(s) = val.to_str()
            {
                req = req.header(name, s);
            }
        }
        if !body.is_empty() {
            req = req.body(body);
        }

        let prepare_started = is_prepare.then(std::time::Instant::now);
        let upstream = req
            .send()
            .await
            .map_err(|e| CompassError::Internal(format!("proxy {safe_path} failed: {}", e.without_url())))?;
        let status = AxumStatusCode::from_u16(upstream.status().as_u16())
            .unwrap_or(AxumStatusCode::INTERNAL_SERVER_ERROR);
        let prepare_headers = is_prepare.then(std::time::Instant::now);
        let upstream_headers = upstream.headers().clone();
        let is_image_archive = upstream_headers.get(header::CONTENT_TYPE)
            .is_some_and(|value| value.as_bytes() == b"application/x-tar");
        let is_attachment = upstream_headers.contains_key(header::CONTENT_DISPOSITION);
        let is_event_stream = upstream_headers.get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"));
        let mut resp = if is_image_archive || is_attachment || is_event_stream {
            let stream = upstream.bytes_stream().map(|chunk| chunk.map_err(reqwest::Error::without_url));
            (status, axum::body::Body::from_stream(stream)).into_response()
        } else {
            let bytes = upstream.bytes().await.map_err(|e|
                CompassError::Internal(format!("read upstream {safe_path} body: {}", e.without_url())))?;
            (status, bytes).into_response()
        };
        for hdr in [
            header::CONTENT_TYPE,
            header::LOCATION,
            header::CONTENT_DISPOSITION,
            header::ETAG,
            axum::http::HeaderName::from_static("x-froglet-file-sha256"),
            axum::http::HeaderName::from_static("x-content-sha256"),
            header::CACHE_CONTROL,
            header::RETRY_AFTER,
            header::REFERRER_POLICY,
            header::CONTENT_SECURITY_POLICY,
            header::X_CONTENT_TYPE_OPTIONS,
        ] {
            if let Some(val) = upstream_headers.get(&hdr)
                && let Ok(hv) = HeaderValue::from_bytes(val.as_bytes())
            {
                resp.headers_mut().insert(hdr, hv);
            }
        }
        if is_shared_file {
            for hdr in [header::CONTENT_LENGTH, header::CONTENT_RANGE, header::ACCEPT_RANGES] {
                if let Some(value) = upstream_headers.get(&hdr) {
                    resp.headers_mut().insert(hdr, value.clone());
                }
            }
        }
        for set_cookie in upstream_headers.get_all(header::SET_COOKIE) {
            if let Ok(hv) = HeaderValue::from_bytes(set_cookie.as_bytes()) {
                resp.headers_mut().append(header::SET_COOKIE, hv);
            }
        }
        if let Some((started, received)) = prepare_started.zip(prepare_headers).filter(|_| status.is_success()) {
            let elapsed_ms = received.duration_since(started).as_secs_f64() * 1000.0;
            let body_ms = received.elapsed().as_secs_f64() * 1000.0;
            let timing = format!("upstream;dur={elapsed_ms:.3}, proxy_body;dur={body_ms:.3}");
            if let Some(hub_timing) = upstream_headers.get("server-timing") {
                resp.headers_mut().append("server-timing", hub_timing.clone());
            }
            if let Ok(value) = HeaderValue::from_str(&timing) {
                resp.headers_mut().append("server-timing", value);
            }
        }
        Ok(resp)
    }

    fn spawn_log_relay(&self, run_id: RunId) {
        // Only Rust callers of subscribe_logs need the broadcast relay. The hosted
        // HTTP event route streams directly, under that request's own credentials.
        if self.log_tx.receiver_count() == 0 {
            return;
        }
        let http = self.http.clone();
        let url = format!(
            "{}/api/v1/runs/{}/events",
            self.cfg.base_url,
            run_id.as_str()
        );
        let auth = current_caller_auth();
        let tx = self.log_tx.clone();
        let observed = tx.clone();
        let relay = async move {
            let Ok(resp) = apply_caller_auth_from(http.get(&url), &auth).send().await else {
                return;
            };
            if !resp.status().is_success() {
                return;
            }
            let mut stream = resp.bytes_stream();
            let mut buf = String::new();
            while let Some(Ok(chunk)) = stream.next().await {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(idx) = buf.find("\n\n") {
                    let frame = buf[..idx].to_owned();
                    buf.drain(..idx + 2);
                    for line in frame.lines() {
                        if let Some(raw) = line.strip_prefix("data:")
                            && let Ok(ev) = serde_json::from_str::<RunLogEvent>(raw.trim())
                        {
                            let _ = tx.send(ev);
                        }
                    }
                }
            }
        };
        tokio::spawn(async move {
            tokio::select! {
                _ = observed.closed() => {}
                _ = relay => {}
            }
        });
    }
}

pub(crate) async fn map_http_error(resp: reqwest::Response) -> CompassError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| format!("hub HTTP {status}: {body}"));

    match status {
        StatusCode::NOT_FOUND => CompassError::NotFound(msg),
        StatusCode::BAD_REQUEST => CompassError::Validation(msg),
        StatusCode::UNAUTHORIZED => {
            let prefixed = if msg.starts_with("UNAUTHENTICATED:") {
                msg
            } else {
                format!("UNAUTHENTICATED: {msg}")
            };
            CompassError::AccessDenied(prefixed)
        }
        StatusCode::FORBIDDEN => CompassError::AccessDenied(msg),
        StatusCode::CONFLICT => CompassError::IntegrityViolation(msg),
        StatusCode::PAYMENT_REQUIRED => CompassError::Payment(msg),
        StatusCode::TOO_MANY_REQUESTS => CompassError::RateLimited(msg),
        _ => CompassError::Internal(msg),
    }
}

async fn get_json<T: serde::de::DeserializeOwned>(
    http: &Client,
    cfg: &CompassHubConfig,
    path: &str,
) -> CompassResult<T> {
    let resp = apply_caller_auth(http.get(format!("{}{}", cfg.base_url, path)))
        .send()
        .await
        .map_err(|e| CompassError::Internal(format!("GET {path} failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(map_http_error(resp).await);
    }
    resp.json::<T>()
        .await
        .map_err(|e| CompassError::Internal(format!("decode GET {path} JSON: {e}")))
}

async fn send_json<T: serde::de::DeserializeOwned>(
    req: reqwest::RequestBuilder,
    label: &str,
) -> CompassResult<T> {
    let resp = apply_caller_auth(req)
        .send()
        .await
        .map_err(|e| CompassError::Internal(format!("{label} failed: {e}")))?;
    decode_hub_json(resp, label).await
}

pub(crate) async fn decode_hub_json<T: serde::de::DeserializeOwned>(
    resp: reqwest::Response, label: &str,
) -> CompassResult<T> {
    if !resp.status().is_success() {
        return Err(map_http_error(resp).await);
    }
    resp.json::<T>()
        .await
        .map_err(|e| CompassError::Internal(format!("decode {label} JSON: {e}")))
}

/// Parse a `BlobStore` key into a tenant-scoped `compass-hub` API path (`B1`).
/// Arbitrary `/api/v1/blobs/*` paths no longer exist.
fn resolve_scoped_blob_get_path(key: &str) -> CompassResult<String> {
    let clean = key.trim_start_matches('/');
    if clean.contains("..") || clean.contains('\\') {
        return Err(CompassError::AccessDenied(format!(
            "FORBIDDEN: unsafe blob key '{key}'"
        )));
    }
    if let Some(rest) = clean.strip_prefix("spills/") {
        let mut parts = rest.split('/');
        if let (Some(session_id), Some(call_id), None) =
            (parts.next(), parts.next(), parts.next())
            && !session_id.is_empty()
            && !call_id.is_empty()
        {
            return Ok(format!("/api/v1/sessions/{session_id}/spills/{call_id}"));
        }
    }
    if let Some(rest) = clean.strip_prefix("runs/") {
        if let Some((run_id, artifact_rel)) = rest.split_once("/artifacts/")
            && !run_id.is_empty()
            && !artifact_rel.is_empty()
            && !artifact_rel.contains('\\')
        {
            return Ok(format!("/api/v1/runs/{run_id}/artifacts/{artifact_rel}"));
        }
        if let Some(run_id) = rest.strip_suffix("/froglet_chain.json")
            && !run_id.is_empty()
            && !run_id.contains('/')
        {
            return Ok(format!("/api/v1/runs/{run_id}/chain"));
        }
    }
    Err(CompassError::AccessDenied(format!(
        "FORBIDDEN: arbitrary blob access ('{key}') is prohibited; use run- or session-scoped routes"
    )))
}

fn resolve_scoped_blob_put_path(key: &str) -> CompassResult<String> {
    let clean = key.trim_start_matches('/');
    if clean.contains("..") || clean.contains('\\') {
        return Err(CompassError::AccessDenied(format!(
            "FORBIDDEN: unsafe blob key '{key}'"
        )));
    }
    if let Some(rest) = clean.strip_prefix("spills/") {
        let mut parts = rest.split('/');
        if let (Some(session_id), Some(call_id), None) =
            (parts.next(), parts.next(), parts.next())
            && !session_id.is_empty()
            && !call_id.is_empty()
        {
            return Ok(format!("/api/v1/sessions/{session_id}/spills/{call_id}"));
        }
    }
    Err(CompassError::AccessDenied(format!(
        "FORBIDDEN: arbitrary blob writes ('{key}') are prohibited in cloud-web"
    )))
}

#[async_trait]
impl SessionStore for CompassHubClient {
    async fn create_session(
        &self,
        _org_id: &OrgId,
        title: &str,
        active_grant_id: &str,
    ) -> CompassResult<SessionRecord> {
        send_json(
            self.http
                .post(format!("{}/api/v1/sessions", self.cfg.base_url))
                .json(&json!({ "title": title, "active_grant_id": active_grant_id })),
            "POST /api/v1/sessions",
        )
        .await
    }

    async fn update_session_grant(
        &self,
        session_id: &SessionId,
        active_grant_id: &str,
    ) -> CompassResult<SessionRecord> {
        self.update_session(session_id, None, Some(active_grant_id), None)
            .await
    }

    async fn update_session(
        &self,
        session_id: &SessionId,
        title: Option<&str>,
        active_grant_id: Option<&str>,
        project_id: Option<Option<&ProjectId>>,
    ) -> CompassResult<SessionRecord> {
        let mut body = serde_json::Map::new();
        if let Some(t) = title {
            body.insert("title".into(), json!(t));
        }
        if let Some(g) = active_grant_id {
            body.insert("active_grant_id".into(), json!(g));
        }
        if let Some(p) = project_id {
            body.insert(
                "project_id".into(),
                p.map(|pid| json!(pid.as_str())).unwrap_or(Value::Null),
            );
        }
        send_json(
            self.http
                .patch(format!(
                    "{}/api/v1/sessions/{}",
                    self.cfg.base_url,
                    session_id.as_str()
                ))
                .json(&Value::Object(body)),
            "PATCH /api/v1/sessions/:id",
        )
        .await
    }

    async fn delete_session(&self, session_id: &SessionId) -> CompassResult<()> {
        let resp = apply_caller_auth(self.http.delete(format!(
            "{}/api/v1/sessions/{}",
            self.cfg.base_url,
            session_id.as_str()
        )))
        .send()
        .await
        .map_err(|e| CompassError::Internal(format!("DELETE /api/v1/sessions/:id: {e}")))?;
        if !resp.status().is_success() {
            return Err(map_http_error(resp).await);
        }
        Ok(())
    }

    async fn get_session(&self, session_id: &SessionId) -> CompassResult<SessionRecord> {
        get_json(
            &self.http,
            &self.cfg,
            &format!("/api/v1/sessions/{}", session_id.as_str()),
        )
        .await
    }

    async fn list_sessions(&self, _org_id: &OrgId) -> CompassResult<Vec<SessionRecord>> {
        #[derive(Deserialize)]
        struct Resp {
            sessions: Vec<SessionRecord>,
        }
        let r: Resp = get_json(&self.http, &self.cfg, "/api/v1/sessions").await?;
        Ok(r.sessions)
    }

    async fn create_project(&self, _org_id: &OrgId, name: &str) -> CompassResult<ProjectRecord> {
        send_json(
            self.http
                .post(format!("{}/api/v1/projects", self.cfg.base_url))
                .json(&json!({ "name": name })),
            "POST /api/v1/projects",
        )
        .await
    }

    async fn rename_project(
        &self,
        project_id: &ProjectId,
        name: &str,
    ) -> CompassResult<ProjectRecord> {
        send_json(
            self.http
                .patch(format!(
                    "{}/api/v1/projects/{}",
                    self.cfg.base_url,
                    project_id.as_str()
                ))
                .json(&json!({ "name": name })),
            "PATCH /api/v1/projects/:id",
        )
        .await
    }

    async fn list_projects(&self, _org_id: &OrgId) -> CompassResult<Vec<ProjectRecord>> {
        #[derive(Deserialize)]
        struct Resp {
            projects: Vec<ProjectRecord>,
        }
        let r: Resp = get_json(&self.http, &self.cfg, "/api/v1/projects").await?;
        Ok(r.projects)
    }

    async fn delete_project(&self, project_id: &ProjectId) -> CompassResult<()> {
        let resp = apply_caller_auth(self.http.delete(format!(
            "{}/api/v1/projects/{}",
            self.cfg.base_url,
            project_id.as_str()
        )))
        .send()
        .await
        .map_err(|e| CompassError::Internal(format!("DELETE /api/v1/projects/:id: {e}")))?;
        if !resp.status().is_success() {
            return Err(map_http_error(resp).await);
        }
        Ok(())
    }

    async fn append_event(
        &self,
        session_id: &SessionId,
        kind: SessionEventKind,
        content: &str,
        metadata: Value,
    ) -> CompassResult<SessionEvent> {
        send_json(
            self.http
                .post(format!(
                    "{}/api/v1/sessions/{}/events",
                    self.cfg.base_url,
                    session_id.as_str()
                ))
                .json(&json!({
                    "kind": kind,
                    "content": content,
                    "metadata": metadata,
                })),
            "POST /api/v1/sessions/:id/events",
        )
        .await
    }

    async fn list_events(&self, session_id: &SessionId) -> CompassResult<Vec<SessionEvent>> {
        #[derive(Deserialize)]
        struct Resp {
            events: Vec<SessionEvent>,
        }
        let r: Resp = get_json(
            &self.http,
            &self.cfg,
            &format!("/api/v1/sessions/{}/events", session_id.as_str()),
        )
        .await?;
        Ok(r.events)
    }

    async fn save_run(&self, _run: &RunRecord) -> CompassResult<()> {
        Err(CompassError::AccessDenied(
            "FORBIDDEN: runs are written exclusively by compass-hub".into(),
        ))
    }

    async fn get_run(&self, run_id: &RunId) -> CompassResult<RunRecord> {
        get_json(
            &self.http,
            &self.cfg,
            &format!("/api/v1/runs/{}", run_id.as_str()),
        )
        .await
    }

    async fn list_runs(&self, _org_id: &OrgId) -> CompassResult<Vec<RunRecord>> {
        #[derive(Deserialize)]
        struct Resp {
            runs: Vec<RunRecord>,
        }
        let r: Resp = get_json(&self.http, &self.cfg, "/api/v1/runs").await?;
        Ok(r.runs)
    }
}

#[async_trait]
impl BlobStore for CompassHubClient {
    async fn put_bytes(&self, key: &str, bytes: &[u8]) -> CompassResult<String> {
        #[derive(Deserialize)]
        struct Resp {
            sha256: String,
        }
        let path = resolve_scoped_blob_put_path(key)?;
        let r: Resp = send_json(
            self.http
                .put(format!("{}{}", self.cfg.base_url, path))
                .body(bytes.to_vec()),
            "PUT /api/v1/sessions/:id/spills/:call_id",
        )
        .await?;
        Ok(r.sha256)
    }

    async fn get_bytes(&self, key: &str) -> CompassResult<Vec<u8>> {
        let path = resolve_scoped_blob_get_path(key)?;
        let resp = apply_caller_auth(self.http.get(format!("{}{}", self.cfg.base_url, path)))
            .send()
            .await
            .map_err(|e| CompassError::Storage(format!("GET scoped blob '{key}': {e}")))?;
        if !resp.status().is_success() {
            return Err(map_http_error(resp).await);
        }
        let b = resp
            .bytes()
            .await
            .map_err(|e| CompassError::Storage(format!("read scoped blob '{key}': {e}")))?;
        Ok(b.to_vec())
    }

    async fn exists(&self, key: &str) -> CompassResult<bool> {
        match self.get_bytes(key).await {
            Ok(_) => Ok(true),
            Err(CompassError::NotFound(_)) => Ok(false),
            Err(err) => Err(err),
        }
    }

    async fn materialize_to_path(&self, key: &str, dest: &Path) -> CompassResult<u64> {
        let data = self.get_bytes(key).await?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CompassError::Storage(format!("mkdir {}: {e}", parent.display())))?;
        }
        std::fs::write(dest, &data)
            .map_err(|e| CompassError::Storage(format!("write {}: {e}", dest.display())))?;
        Ok(data.len() as u64)
    }
}

#[async_trait]
impl PaymentGateway for CompassHubClient {
    async fn available_balance(&self, _org_id: &OrgId) -> CompassResult<CreditAmountAct> {
        #[derive(Deserialize)]
        struct Resp {
            available_milli_act: i64,
        }
        let r: Resp = get_json(&self.http, &self.cfg, "/api/v1/ledger").await?;
        Ok(CreditAmountAct::from_milli_act(r.available_milli_act))
    }

    async fn reserve_for_run(
        &self,
        _org_id: &OrgId,
        _run_id: &RunId,
        _amount: CreditAmountAct,
        _idempotency_key: &str,
    ) -> CompassResult<LedgerTx> {
        Err(CompassError::Internal(
            "reserve_for_run is executed atomically inside compass-hub confirm_run".into(),
        ))
    }

    async fn settle_run(
        &self,
        _org_id: &OrgId,
        _run_id: &RunId,
        _receipt_hash: &str,
    ) -> CompassResult<LedgerTx> {
        Err(CompassError::Internal(
            "settle_run is executed atomically inside compass-hub confirm_run".into(),
        ))
    }

    async fn refund_run(
        &self,
        _org_id: &OrgId,
        _run_id: &RunId,
        _reason: &str,
    ) -> CompassResult<LedgerTx> {
        Err(CompassError::Internal(
            "refund_run is executed atomically inside compass-hub confirm_run".into(),
        ))
    }

    async fn credit_topup(
        &self,
        _org_id: &OrgId,
        amount: CreditAmountAct,
        reference: &str,
        _via_webhook: bool,
    ) -> CompassResult<LedgerTx> {
        send_json(
            self.http
                .post(format!("{}/api/v1/billing/invoices/credit", self.cfg.base_url))
                .json(&json!({
                    "amount_milli_act": amount.milli_act(),
                    "reference": reference,
                })),
            "POST /api/v1/billing/invoices/credit",
        )
        .await
    }

    async fn list_transactions(&self, _org_id: &OrgId) -> CompassResult<Vec<LedgerTx>> {
        #[derive(Deserialize)]
        struct Resp {
            transactions: Vec<LedgerTx>,
        }
        let r: Resp = get_json(&self.http, &self.cfg, "/api/v1/ledger").await?;
        Ok(r.transactions)
    }
}

impl CompassHubClient {
    pub(crate) async fn request_bound_quote(
        &self,
        req: &PrepareRunRequest,
        execution_id: Option<uuid::Uuid>,
        workspace_id: Option<uuid::Uuid>,
        request_key: Option<uuid::Uuid>,
    ) -> CompassResult<reqwest::Response> {
        biocompass_core_traits::validate_requested_plugin_version(req.plugin_version.as_deref())?;
        let mut body = json!({
            "plugin_id": req.plugin_id,
            "offer_id": req.offer_id,
            "params": req.params,
            "grant_id": req.dataset_grant.as_ref().map(|g| g.grant_id.as_str()),
            "session_id": req.session_id,
        });
        if let Some(id) = execution_id {
            body["agent_execution_id"] = json!(id);
        }
        if let Some(id) = workspace_id {
            body["workspace_id"] = json!(id);
        }
        if let Some(key) = request_key {
            body["request_key"] = json!(key);
        }
        if let Some(version) = &req.plugin_version {
            body["plugin_version"] = json!(version);
        }
        apply_caller_auth(self.http.post(format!("{}/api/v1/runs/prepare", self.cfg.base_url)).json(&body))
            .send().await.map_err(|e| CompassError::Internal(
                format!("POST /api/v1/runs/prepare failed: {}", e.without_url()),
            ))
    }

    pub(crate) async fn read_bound_quote(
        req: &PrepareRunRequest, response: reqwest::Response,
    ) -> CompassResult<RunQuote> {
        let quote: RunQuote = decode_hub_json(response, "POST /api/v1/runs/prepare").await?;
        quote.validate_selection(
            &req.plugin_id,
            req.plugin_version.as_deref(),
            req.offer_id.as_deref(),
        )?;
        Ok(quote)
    }

    async fn prepare_bound_quote(&self, req: PrepareRunRequest, execution_id: Option<uuid::Uuid>) -> CompassResult<RunQuote> {
        // In-process agents and SDK callers share the HTTP gateway's quote limit.
        let _permit = self.try_prepare_slot()?;
        let response = self.request_bound_quote(&req, execution_id, None, None).await?;
        Self::read_bound_quote(&req, response).await
    }

    pub(crate) async fn submit_confirmation(&self, path: &str, key: &str) -> CompassResult<reqwest::Response> {
        apply_caller_auth(self.http.post(format!("{}{path}", self.cfg.base_url))
            .json(&json!({ "idempotency_key": key })))
            .send().await.map_err(|e| CompassError::Internal(
                format!("POST /api/v1/runs/:id/confirm failed: {}", e.without_url()),
            ))
    }
}

#[async_trait]
impl RunDispatcher for CompassHubClient {
    async fn prepare_run(&self, req: PrepareRunRequest) -> CompassResult<RunQuote> {
        self.prepare_bound_quote(req, None).await
    }

    async fn confirm_run(&self, run_id: &RunId, idempotency_key: &str) -> CompassResult<RunRecord> {
        self.spawn_log_relay(run_id.clone());
        let upstream = self.submit_confirmation(
            &format!("/api/v1/runs/{}/confirm", run_id.as_str()), idempotency_key,
        ).await?;
        decode_hub_json(upstream, "POST /api/v1/runs/:id/confirm").await
    }

    fn subscribe_logs(&self) -> broadcast::Receiver<RunLogEvent> {
        self.log_tx.subscribe()
    }
}

#[async_trait]
impl WorkspaceRegistryPort for CompassHubClient {
    fn node_id(&self) -> NodeId {
        self.node_id.clone()
    }

    fn provider_id(&self) -> String {
        self.provider_id.clone()
    }

    fn requester_id(&self) -> String {
        self.requester_id.clone()
    }

    async fn list_catalog_manifests(&self) -> CompassResult<Vec<CatalogSummary>> {
        #[derive(Deserialize)]
        struct Resp {
            catalogs: Vec<CatalogSummary>,
        }
        let r: Resp = get_json(&self.http, &self.cfg, "/api/v1/catalogs").await?;
        Ok(r.catalogs)
    }

    async fn query_catalog(
        &self,
        id: &CatalogId,
        grant: &DatasetGrant,
        query: &QueryAst,
    ) -> CompassResult<QueryTableResult> {
        send_json(
            self.http
                .post(format!(
                    "{}/api/v1/catalogs/{}/query",
                    self.cfg.base_url,
                    id.as_str()
                ))
                .json(&json!({
                    "grant_id": grant.grant_id,
                    "query": query,
                })),
            "POST /api/v1/catalogs/:ns/:name/query",
        )
        .await
    }

    async fn rollback_catalog_snapshot(
        &self,
        id: &CatalogId,
        target_snapshot_id: u64,
    ) -> CompassResult<CatalogSummary> {
        #[derive(Deserialize)]
        struct Resp {
            catalog: CatalogSummary,
        }
        let r: Resp = send_json(
            self.http
                .post(format!(
                    "{}/api/v1/catalogs/{}/rollback",
                    self.cfg.base_url,
                    id.as_str()
                ))
                .json(&json!({ "target_snapshot_id": target_snapshot_id })),
            "POST /api/v1/catalogs/:ns/:name/rollback",
        )
        .await?;
        Ok(r.catalog)
    }

    async fn set_catalog_snapshot_pin(
        &self,
        id: &CatalogId,
        snapshot_id: u64,
        pinned: bool,
    ) -> CompassResult<CatalogSummary> {
        #[derive(Deserialize)]
        struct Resp {
            catalog: CatalogSummary,
        }
        let r: Resp = send_json(
            self.http
                .post(format!(
                    "{}/api/v1/catalogs/{}/snapshots/{snapshot_id}/pin",
                    self.cfg.base_url,
                    id.as_str()
                ))
                .json(&json!({ "pinned": pinned })),
            "POST /api/v1/catalogs/:ns/:name/snapshots/:sid/pin",
        )
        .await?;
        Ok(r.catalog)
    }

    async fn list_plugins(&self) -> CompassResult<Vec<LoadedPlugin>> {
        Ok(self.list_plugins_with_ui().await?.into_iter().map(|(plugin, _)| plugin).collect())
    }

    async fn get_plugin(&self, id: &PluginId) -> CompassResult<LoadedPlugin> {
        {
            let ui_roots = self.local_ui_roots.read().await;
            if let Some(root) = ui_roots.get(id)
                && let Ok(loaded) = LoadedPlugin::load_from_dir(root, true)
            {
                // If caller is unauthenticated (e.g., static /plugin-ui asset load for bundled plugin),
                // prefer hub visibility check when authenticated, or fall back to bundled UI asset metadata.
                if let Ok(plugins) = WorkspaceRegistryPort::list_plugins(self).await
                    && let Some(found) = plugins.into_iter().find(|p| &p.manifest.id == id)
                {
                    return Ok(found);
                }
                return Ok(loaded);
            }
        }
        WorkspaceRegistryPort::list_plugins(self)
            .await?
            .into_iter()
            .find(|p| &p.manifest.id == id)
            .ok_or_else(|| CompassError::NotFound(format!("plugin '{id}' not found on hub")))
    }

    async fn register_plugin(&self, plugin: LoadedPlugin) -> CompassResult<()> {
        if let Some(root) = &plugin.root_dir {
            self.local_ui_roots
                .write()
                .await
                .insert(plugin.manifest.id.clone(), root.clone());
        }
        let _: Value = send_json(
            self.http
                .post(format!("{}/api/v1/plugins", self.cfg.base_url))
                .json(&HubPluginDto {
                    manifest: plugin.manifest,
                    skill: plugin.skill,
                    visibility: None,
                    has_ui_bundle: None,
                }),
            "POST /api/v1/plugins",
        )
        .await?;
        Ok(())
    }

    async fn list_grants(
        &self,
        _org_id: &OrgId,
        _default_cat: &CatalogId,
    ) -> CompassResult<Vec<DatasetGrant>> {
        self.fetch_grants().await
    }
}

#[async_trait]
impl AgentExecutionPort for CompassHubClient {
    async fn list_plugins(&self) -> Vec<LoadedPlugin> {
        WorkspaceRegistryPort::list_plugins(self)
            .await
            .unwrap_or_default()
    }

    async fn list_catalog_manifests(&self) -> Vec<CatalogSummary> {
        WorkspaceRegistryPort::list_catalog_manifests(self)
            .await
            .unwrap_or_default()
    }

    async fn query_catalog(
        &self,
        id: &CatalogId,
        grant: &DatasetGrant,
        query: &QueryAst,
    ) -> CompassResult<QueryTableResult> {
        WorkspaceRegistryPort::query_catalog(self, id, grant, query).await
    }

    async fn prepare_plugin_quote(&self, req: PrepareRunRequest) -> CompassResult<(RunQuote, Option<biocompass_agent_core::tools::AgentAdmission>)> {
        Ok((self.prepare_run(req).await?, None))
    }
}
