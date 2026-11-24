//! Authenticated hosted execution from a personal, loopback-only installed workspace.
mod saved_sessions;

use crate::cloud_client::{CallerAuth, CALLER_AUTH, apply_workspace_auth, current_caller_auth, map_http_error};
use crate::server::ApiResult;
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use biocompass_core_traits::local_delivery::{
    DeliveryFile, DeliveryScope, LocalDelivery, LocalDeliveryStore, PendingDelivery,
};
use biocompass_core_traits::{
    CatalogId, CompassError, CompassResult, OrgId, PluginId, RunId, RunQuote, RunRecord, RunStatus,
};
use biocompass_node_runtime::verify_released_artifact;
use chrono::Utc;
use reqwest::{Client, Method, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path as FsPath, time::Duration};
use std::sync::Arc;
use tokio::sync::Mutex;

const SYNC_INTERVAL_SECONDS: u64 = 60;

pub struct InstalledWorkspace {
    base: Url,
    client: Client,
    store: LocalDeliveryStore,
    sync_session: Mutex<Option<String>>,
    sync_lock: Mutex<()>,
}

#[derive(Deserialize)]
struct Principal {
    org_id: OrgId,
    user_id: Option<String>,
    actor: String,
    auth_method: String,
    session_expires_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Deserialize, Serialize)]
struct SourceSnapshot {
    source_name: String,
    catalog_id: CatalogId,
    snapshot_id: u64,
    dataset_digest: String,
}

#[derive(Deserialize)]
struct Manifest {
    workspace_id: String,
    run: RunRecord,
    source_snapshots: Vec<SourceSnapshot>,
    receipt_hash: String,
    result_hash: String,
}

#[derive(Deserialize)]
struct DeliveryPage {
    deliveries: Vec<DeliveryId>,
    next_after: Option<String>,
}

#[derive(Deserialize)]
struct DeliveryId {
    run_id: RunId,
    receipt_hash: String,
    result_hash: String,
}

#[derive(Deserialize)]
struct Acknowledgement {
    workspace_id: String,
    run_id: RunId,
    receipt_hash: String,
    result_hash: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Prepare {
    plugin_id: PluginId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plugin_version: Option<String>,
    offer_id: Option<String>,
    #[serde(default = "empty_params")]
    params: Value,
    grant_id: Option<String>,
}

fn empty_params() -> Value {
    json!({})
}

#[derive(Deserialize)]
struct Confirm {
    idempotency_key: Option<String>,
}

impl InstalledWorkspace {
    pub(crate) fn hub_url(&self) -> &str {
        self.base.as_str().trim_end_matches('/')
    }

    pub fn open(data_dir: &FsPath, hub_url: &str) -> CompassResult<Self> {
        let base = Url::parse(hub_url.trim_end_matches('/'))
            .map_err(|e| CompassError::Validation(format!("invalid COMPASS_HUB_URL: {e}")))?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(CompassError::Validation(
                "hub URL must be an HTTP(S) base without embedded credentials, query or fragment"
                    .into(),
            ));
        }
        let host = base.host_str().expect("validated HTTP host");
        let loopback = host.trim_end_matches('.') == "localhost"
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if base.scheme() != "https" && !loopback {
            return Err(CompassError::Validation(
                "a hosted workspace requires HTTPS outside loopback".into(),
            ));
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| CompassError::Internal(format!("build workspace HTTP client: {e}")))?;
        Ok(Self {
            base,
            client,
            store: LocalDeliveryStore::open_workspace(data_dir)?,
            sync_session: Mutex::new(None),
            sync_lock: Mutex::new(()),
        })
    }

    /// Keep the personal installation's pickup timer independent of an open browser tab.
    pub fn start_sync(self: &Arc<Self>) {
        let workspace = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut clock = tokio::time::interval(Duration::from_secs(SYNC_INTERVAL_SECONDS));
            clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                clock.tick().await;
                let Some(workspace) = workspace.upgrade() else { break };
                let authorization = workspace.sync_session.lock().await.clone();
                let Some(authorization) = authorization else { continue };
                let auth = CallerAuth { authorization: Some(authorization), ..CallerAuth::default() };
                let outcome = CALLER_AUTH.scope(auth, workspace.scheduled_sync()).await;
                if let Err(error) = outcome {
                    tracing::debug!(error = %error, "workspace pickup deferred");
                }
            }
        });
    }

    async fn scheduled_sync(&self) -> CompassResult<()> {
        let profile: Value = self.json(self.request(Method::GET, &["workspace", "settings"])).await?;
        if self.matches_profile(&profile)
            && (profile["sync_enabled"] == true || profile["default_location"] == "local")
        {
            self.sync_profile(&self.scope().await?, true).await?;
        }
        Ok(())
    }

    fn matches_profile(&self, profile: &Value) -> bool {
        profile["local_workspace_id"].as_str() == Some(self.store.workspace_id())
    }

    async fn sync_profile(&self, scope: &DeliveryScope, paired: bool) -> CompassResult<Value> {
        let _single_flight = self.sync_lock.lock().await;
        if paired {
            self.discover_queued_deliveries(scope).await?;
        }
        let mut result = self.sync(scope).await?;
        if paired && result.get("errors").and_then(Value::as_array).is_some_and(Vec::is_empty) {
            let profile: Value = self.json(self.request(Method::POST,
                &["workspaces", self.store.workspace_id(), "synced"])).await?;
            result["last_synced_at"] = profile["last_synced_at"].clone();
        }
        Ok(result)
    }

    fn unsigned_request(&self, method: Method, path: &[&str]) -> reqwest::RequestBuilder {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("HTTP(S) base URL")
            .pop_if_empty()
            .extend(["api", "v1"])
            .extend(path.iter().copied());
        self.client.request(method, url)
    }

    pub(crate) fn request(&self, method: Method, path: &[&str]) -> reqwest::RequestBuilder {
        apply_workspace_auth(self.unsigned_request(method, path))
    }

    pub(crate) async fn send(&self, request: reqwest::RequestBuilder) -> CompassResult<reqwest::Response> {
        let reply = request
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() || e.is_timeout() {
                    CompassError::Storage("The home hub is unreachable. Reconnect to access your workspace account.".into())
                } else {
                    CompassError::Storage(format!("workspace hub request failed: {e}"))
                }
            })?;
        if !reply.status().is_success() {
            let identity_endpoint = reply.url().path().ends_with("/auth/whoami")
                || reply.url().path().ends_with("/workspace/settings");
            if reply.status() == StatusCode::UNAUTHORIZED || (identity_endpoint && reply.status() == StatusCode::FORBIDDEN) {
                self.forget_browser_session(&HeaderMap::new(), true).await?;
            }
            return Err(map_http_error(reply).await);
        }
        Ok(reply)
    }

    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> CompassResult<T> {
        self.send(request)
            .await?
            .json()
            .await
            .map_err(|e| CompassError::Protocol(format!("invalid workspace hub response: {e}")))
    }

    async fn tracked_run(
        &self,
        id: &RunId,
        request: reqwest::RequestBuilder,
    ) -> CompassResult<RunRecord> {
        let scope = self.scope().await?;
        let tracked = self.store.get_pending(&scope, id).await?;
        let run: RunRecord = self.json(request).await?;
        check_run(&scope, &tracked.quote, &run)?;
        Ok(run)
    }

    pub(crate) async fn scope(&self) -> CompassResult<DeliveryScope> {
        let p: Principal = self
            .json(self.request(Method::GET, &["auth", "whoami"]))
            .await?;
        let subject = if let Some(id) = p.user_id.filter(|id| !id.is_empty()) {
            format!("user:{id}")
        } else if p.auth_method == "api_key" {
            p.actor
                .strip_prefix("apikey:")
                .and_then(|actor| actor.split_once('('))
                .filter(|(id, _)| !id.is_empty())
                .map(|(id, _)| format!("apikey:{id}"))
                .ok_or_else(|| {
                    CompassError::Protocol("hub did not return a stable API-key identity".into())
                })?
        } else {
            return Err(CompassError::AccessDenied(
                "hub principal has no workspace subject".into(),
            ));
        };
        Ok(DeliveryScope {
            origin: self.base.as_str().trim_end_matches('/').to_string(),
            org_id: p.org_id,
            subject,
        })
    }

    pub(crate) async fn bounded_bytes(&self, path: &[&str], cap: u64) -> CompassResult<Vec<u8>> {
        let mut response = self.send(self.request(Method::GET, path)).await?;
        if response.content_length().is_some_and(|size| size > cap) {
            return Err(CompassError::IntegrityViolation(
                "workspace file exceeds its signed output limit".into(),
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| CompassError::Storage(format!("interrupted workspace download: {e}")))?
        {
            if bytes.len() as u64 + chunk.len() as u64 > cap {
                return Err(CompassError::IntegrityViolation(
                    "workspace file exceeds its signed output limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    async fn fetch(
        &self,
        scope: &DeliveryScope,
        tracked: &PendingDelivery,
    ) -> CompassResult<Option<LocalDelivery>> {
        let run_id = &tracked.quote.run_id;
        let prefix = [
            "workspaces",
            self.store.workspace_id(),
            "deliveries",
            run_id.as_str(),
        ];
        let manifest: Manifest = match self.json(self.request(Method::GET, &prefix)).await {
            Ok(value) => value,
            Err(CompassError::NotFound(_)) => {
                let run: RunRecord = self
                    .json(self.request(Method::GET, &["runs", run_id.as_str()]))
                    .await?;
                check_run(scope, &tracked.quote, &run)?;
                if matches!(run.status, RunStatus::Failed | RunStatus::Canceled | RunStatus::Succeeded)
                    || (run.status == RunStatus::Prepared && run.quote.expires_at <= Utc::now())
                {
                    self.store.retire_without_result(scope, run_id).await?;
                }
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        check_run(scope, &tracked.quote, &manifest.run)?;
        let run = &manifest.run;
        let receipt = run
            .froglet_receipt
            .as_ref()
            .ok_or_else(|| CompassError::IntegrityViolation("missing delivery receipt".into()))?;
        if manifest.workspace_id != self.store.workspace_id()
            || receipt.get("hash").and_then(Value::as_str) != Some(manifest.receipt_hash.as_str())
            || receipt
                .pointer("/payload/result_hash")
                .and_then(Value::as_str)
                != Some(manifest.result_hash.as_str())
        {
            return Err(CompassError::IntegrityViolation(
                "delivery manifest disagrees with workspace or receipt".into(),
            ));
        }
        let primary = match manifest.source_snapshots.as_slice() {
            [] => None,
            [s] if s.source_name == "primary" => {
                Some((&s.catalog_id, s.snapshot_id, s.dataset_digest.as_str()))
            }
            _ => {
                return Err(CompassError::IntegrityViolation(
                    "unexpected delivery snapshot bindings".into(),
                ));
            }
        };
        let cap = tracked
            .quote
            .froglet_quote
            .pointer("/payload/execution_limits/max_output_bytes")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                CompassError::IntegrityViolation("signed quote has no output size limit".into())
            })?;
        let files_size = run
            .artifacts
            .iter()
            .try_fold(0u64, |size, file| size.checked_add(file.size_bytes))
            .ok_or_else(|| CompassError::IntegrityViolation("artifact size overflow".into()))?;
        let result_cap = cap.checked_sub(files_size).ok_or_else(|| {
            CompassError::IntegrityViolation("artifacts exceed quoted output limit".into())
        })?;
        let result = self
            .bounded_bytes(
                &[prefix[0], prefix[1], prefix[2], prefix[3], "result"],
                result_cap,
            )
            .await?;
        // W6-2: compare signed parent/child links with what the hub brokered, not with themselves.
        let links = if biocompass_node_runtime::BrokeredRunLinks::required_for(run) {
            let bundle: Value = self
                .json(self.request(Method::GET, &["runs", run_id.as_str(), "provenance"]))
                .await?;
            biocompass_node_runtime::BrokeredRunLinks::from_provenance_bundle(
                &bundle,
                run_id.as_str(),
                run.parent_run_id.as_deref(),
            )
        } else {
            biocompass_node_runtime::BrokeredRunLinks::default()
        };
        biocompass_node_runtime::verify_released_result_with_links(run, &result, primary, &links)?;
        let mut files = Vec::with_capacity(run.artifacts.len());
        for (index, artifact) in run.artifacts.iter().enumerate() {
            let index = index.to_string();
            let bytes = self
                .bounded_bytes(
                    &[
                        prefix[0],
                        prefix[1],
                        prefix[2],
                        prefix[3],
                        "artifacts",
                        &index,
                    ],
                    artifact.size_bytes,
                )
                .await?;
            verify_released_artifact(
                artifact,
                &hex::encode(Sha256::digest(&bytes)),
                bytes.len() as u64,
            )?;
            files.push(DeliveryFile {
                name: artifact.name.clone(),
                bytes,
            });
        }
        let provenance = json!({ "source_snapshots": manifest.source_snapshots });
        self.store
            .publish(
                scope,
                run,
                &manifest.receipt_hash,
                &hex::encode(Sha256::digest(&result)),
                &provenance,
                &result,
                &files,
            )
            .await
            .map(Some)
    }

    async fn acknowledge(&self, scope: &DeliveryScope, saved: &LocalDelivery) -> CompassResult<()> {
        let id = &saved.run.run_id;
        let result_hash = saved
            .run
            .froglet_receipt
            .as_ref()
            .and_then(|r| r.pointer("/payload/result_hash"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CompassError::IntegrityViolation("saved receipt lacks result hash".into())
            })?;
        let ack: Acknowledgement = self
            .json(
                self.request(
                    Method::POST,
                    &[
                        "workspaces",
                        self.store.workspace_id(),
                        "deliveries",
                        id.as_str(),
                        "ack",
                    ],
                )
                .json(&json!({"receipt_hash":saved.receipt_hash,"result_hash":result_hash})),
            )
            .await?;
        if ack.workspace_id != self.store.workspace_id()
            || ack.run_id != *id
            || ack.receipt_hash != saved.receipt_hash
            || ack.result_hash != result_hash
        {
            return Err(CompassError::IntegrityViolation(
                "hub acknowledged a different delivery".into(),
            ));
        }
        self.store.acknowledge(scope, id, &saved.receipt_hash).await
    }

    async fn sync(&self, scope: &DeliveryScope) -> CompassResult<Value> {
        let mut delivered = 0;
        let mut acknowledged = 0;
        let mut errors = Vec::new();
        for tracked in self.store.pending(scope).await? {
            let id = &tracked.quote.run_id;
            let item: CompassResult<()> = async {
                let saved = if tracked.saved {
                    match self.store.verify_saved(scope, id).await {
                        Ok(verified) => Some(verified),
                        Err(_) => self.fetch(scope, &tracked).await?,
                    }
                } else {
                    let result = self.fetch(scope, &tracked).await?;
                    if result.is_some() {
                        delivered += 1;
                    }
                    result
                };
                if let Some(saved) = saved {
                    self.acknowledge(scope, &saved).await?;
                    acknowledged += 1;
                }
                Ok(())
            }
            .await;
            if let Err(error) = item {
                errors.push(json!({"run_id":id,"error":error.to_string()}));
            }
        }
        let pending = self.store.pending(scope).await?.len();
        Ok(
            json!({"delivered":delivered,"acknowledged":acknowledged,"pending":pending,"errors":errors}),
        )
    }

    async fn discover_queued_deliveries(&self, scope: &DeliveryScope) -> CompassResult<()> {
        let base = ["workspaces", self.store.workspace_id(), "deliveries"];
        let mut after = String::new();
        loop {
            let page: DeliveryPage = self.json(self.request(Method::GET, &base).query(&[("after", &after)])).await?;
            for item in page.deliveries {
                let manifest: Manifest = self.json(self.request(Method::GET,
                    &[base[0], base[1], base[2], item.run_id.as_str()])).await?;
                if manifest.workspace_id != self.store.workspace_id() || item.run_id != manifest.run.run_id
                    || item.receipt_hash != manifest.receipt_hash || item.result_hash != manifest.result_hash {
                    return Err(CompassError::IntegrityViolation("workspace delivery changed during discovery".into()));
                }
                check_run(scope, &manifest.run.quote, &manifest.run)?;
                verify_workspace_quote(&manifest.run.quote)?;
                self.store.register_pending(scope, &manifest.run.quote).await?;
            }
            match page.next_after {
                Some(next) if next > after => after = next,
                Some(_) => return Err(CompassError::Protocol("workspace inbox cursor did not advance".into())),
                None => return Ok(()),
            }
        }
    }
}

fn verify_workspace_quote(quote: &RunQuote) -> CompassResult<()> {
    if froglet_verify::verify_document(&quote.froglet_quote, None).status
            != froglet_verify::DocumentStatus::Verified
        || quote.froglet_quote.pointer("/payload/workload_hash").and_then(Value::as_str)
            != Some(quote.workload_hash.as_str())
    {
        return Err(CompassError::IntegrityViolation("hosted quote signature, module or workload mismatch".into()));
    }
    Ok(())
}

fn check_run(scope: &DeliveryScope, quote: &RunQuote, run: &RunRecord) -> CompassResult<()> {
    let json = |q: &RunQuote| {
        serde_json::to_value(q)
            .map_err(|e| CompassError::Protocol(format!("serialize original workspace quote: {e}")))
    };
    if run.org_id != scope.org_id
        || run.run_id != quote.run_id
        || run.plugin_id != quote.plugin_id
        || run.plugin_version != quote.plugin_version
        || json(&run.quote)? != json(quote)?
    {
        return Err(CompassError::IntegrityViolation(
            "hosted run differs from the original workspace quote or organization".into(),
        ));
    }
    Ok(())
}

fn connected(state: &AppState) -> CompassResult<&InstalledWorkspace> {
    state.installed_workspace.as_deref().ok_or_else(||CompassError::AccessDenied(
        "hosted workspace access requires a personal loopback installation with COMPASS_HUB_URL configured".into()))
}
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/workspace/settings", get(settings).put(settings))
        .route("/api/v1/workspace/pair", post(pair))
        .route("/api/v1/workspace/sync-session", delete(end_sync_session))
        .route("/api/v1/workspace/identity", get(identity))
        .route("/api/v1/workspace/modules", get(module_downloads))
        .route("/api/v1/workspace/auth/login", post(login_for_downloads))
        .route("/api/v1/workspace/runs/prepare", post(prepare))
        .route("/api/v1/workspace/runs/:id/confirm", post(confirm))
        .route("/api/v1/workspace/runs/:id/status", get(hosted_status))
        .route("/api/v1/workspace/runs/:id/cancel", post(cancel))
        .route("/api/v1/workspace/sync", post(sync))
        .route("/api/v1/workspace/runs", get(list))
        .route("/api/v1/workspace/runs/:id", get(detail))
        .route("/api/v1/workspace/runs/:id/result", get(result))
        .route("/api/v1/workspace/runs/:id/artifacts/*name", get(artifact))
        .layer(axum::middleware::map_response(|mut response: Response| async move {
            response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
            response
        }))
}

async fn settings(
    State(state): State<AppState>, method: axum::http::Method, headers: HeaderMap, body: Bytes,
) -> ApiResult<Response> {
    if let Some(hub) = state.hub_client.as_ref() {
        return Ok(hub.proxy_raw(method, "/api/v1/workspace/settings", &headers, body.to_vec()).await?);
    }
    if let Some(w) = state.installed_workspace.as_deref() {
        let request = w.browser_request(method, &["workspace", "settings"]);
        let request = if !body.is_empty() {
            request.header(header::CONTENT_TYPE, "application/json").body(body)
        } else { request };
        let mut payload: Value = w.json(request).await?;
        w.remember_browser_session(&mut payload).await?;
        if w.matches_profile(&payload) {
            if let Some(token) = current_caller_auth().authorization.filter(|v| v.starts_with("Bearer ")) {
                *w.sync_session.lock().await = Some(token);
            }
        } else if payload["local_workspace_id"].is_string() {
            payload["available_locations"] = json!(["cloud"]);
        }
        return Ok(([(header::CACHE_CONTROL, "no-store")], Json(payload)).into_response());
    }
    if method != Method::GET {
        return Err(CompassError::Validation("This installation has only a local workspace.".into()).into());
    }
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(json!({
        "available_locations":["local"], "local_workspace_id":null, "default_location":"local",
        "sync_enabled":false, "sync_interval_seconds":null, "last_synced_at":null,
    }))).into_response())
}

async fn pair(State(state): State<AppState>) -> ApiResult<Response> {
    let w = connected(&state)?;
    let mut payload: Value = w.json(w.browser_request(Method::POST, &["workspaces", w.store.workspace_id(), "pair"])).await?;
    w.remember_browser_session(&mut payload).await?;
    if let Some(token) = current_caller_auth().authorization.filter(|v| v.starts_with("Bearer ")) {
        *w.sync_session.lock().await = Some(token);
    }
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(payload)).into_response())
}

async fn end_sync_session(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<StatusCode> {
    let w = connected(&state)?;
    w.forget_browser_session(&headers, true).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceLogin {
    email: String,
    password: String,
    org_id: Option<String>,
    totp_code: Option<String>,
}

async fn login_for_downloads(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(login): Json<WorkspaceLogin>,
) -> ApiResult<Response> {
    let workspace = connected(&state)?;
    #[derive(Deserialize, Serialize)]
    struct LoginSession { session_token: String }
    let session: LoginSession = workspace.json(
        workspace.request(Method::POST, &["auth", "login"]).json(&login)
    ).await?;
    workspace.forget_browser_session(&headers, false).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(session)).into_response())
}

async fn module_downloads(State(state): State<AppState>) -> ApiResult<Response> {
    let workspace = connected(&state)?;
    let downloads: Value = workspace.json(
        workspace.request(Method::GET, &["plugins", "downloads"])
    ).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(downloads)).into_response())
}

async fn identity(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let w = connected(&state)?;
    let scope = w.scope().await?;
    Ok(Json(
        json!({"origin":scope.origin,"org_id":scope.org_id,"subject":scope.subject,"workspace_id":w.store.workspace_id()}),
    ))
}

async fn prepare(
    State(state): State<AppState>,
    Json(req): Json<Prepare>,
) -> ApiResult<Json<RunQuote>> {
    biocompass_core_traits::validate_requested_plugin_version(req.plugin_version.as_deref())?;
    let w = connected(&state)?;
    let scope = w.scope().await?;
    let mut payload = json!(&req);
    payload["workspace_id"] = json!(w.store.workspace_id());
    let quote: RunQuote = w
        .json(w.request(Method::POST, &["runs", "prepare"]).json(&payload))
        .await?;
    quote.validate_selection(
        &req.plugin_id,
        req.plugin_version.as_deref(),
        req.offer_id.as_deref(),
    )?;
    verify_workspace_quote(&quote)?;
    w.store.register_pending(&scope, &quote).await?;
    Ok(Json(quote))
}

async fn confirm(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Confirm>>,
) -> ApiResult<(StatusCode, Json<RunRecord>)> {
    let id = RunId::new(id).map_err(CompassError::Validation)?;
    let idem = body
        .and_then(|b| b.0.idempotency_key)
        .filter(|key| !key.trim().is_empty())
        .unwrap_or_else(|| format!("idem-{id}"));
    let w = connected(&state)?;
    let run = w
        .tracked_run(
            &id,
            w.request(Method::POST, &["runs", id.as_str(), "confirm"])
                .json(&json!({"idempotency_key":idem})),
        )
        .await?;
    let status = if run.confirmation_pending() {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(run)))
}

async fn hosted_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<RunRecord>> {
    let w = connected(&state)?;
    let id = RunId::new(id).map_err(CompassError::Validation)?;
    Ok(Json(
        w.tracked_run(&id, w.request(Method::GET, &["runs", id.as_str()]))
            .await?,
    ))
}

async fn cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<RunRecord>> {
    let w = connected(&state)?;
    let id = RunId::new(id).map_err(CompassError::Validation)?;
    Ok(Json(
        w.tracked_run(
            &id,
            w.request(Method::POST, &["runs", id.as_str(), "cancel"]),
        )
        .await?,
    ))
}

async fn sync(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let w = connected(&state)?;
    let scope = w.scope().await?;
    let settings: CompassResult<Value> = w.json(w.request(Method::GET, &["workspace", "settings"])).await;
    let paired = match settings {
        Ok(value) => w.matches_profile(&value),
        // Older servers support explicitly addressed pickup without a pairing screen.
        Err(CompassError::NotFound(_) | CompassError::AccessDenied(_)) => false,
        Err(error) => return Err(error.into()),
    };
    Ok(Json(w.sync_profile(&scope, paired).await?))
}
async fn list(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let w = connected(&state)?;
    let access = w.saved_read_access(&headers).await?;
    let scope = access.scope;
    let runs = w
        .store
        .list(&scope)
        .await?
        .into_iter()
        .map(|d| d.run)
        .collect::<Vec<_>>();
    Ok(Json(json!({"runs":runs,"offline":access.offline,"read_session_expires_at":access.expires_at})))
}
async fn detail(
    State(state): State<AppState>,
    Path(id): Path<String>, headers: HeaderMap,
) -> ApiResult<Json<RunRecord>> {
    let w = connected(&state)?;
    let scope = w.saved_read_access(&headers).await?.scope;
    let id = RunId::new(id).map_err(CompassError::Validation)?;
    Ok(Json(w.store.get(&scope, &id).await?.run))
}
async fn result(State(state): State<AppState>, Path(id): Path<String>, headers: HeaderMap) -> ApiResult<Response> {
    let w = connected(&state)?;
    let scope = w.saved_read_access(&headers).await?.scope;
    let id = RunId::new(id).map_err(CompassError::Validation)?;
    Ok(file_response(
        w.store.result_bytes(&scope, &id).await?,
        "application/json",
    ))
}
async fn artifact(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>, headers: HeaderMap,
) -> ApiResult<Response> {
    let w = connected(&state)?;
    let scope = w.saved_read_access(&headers).await?.scope;
    let id = RunId::new(id).map_err(CompassError::Validation)?;
    let saved = w.store.get(&scope, &id).await?;
    let file = saved
        .run
        .artifacts
        .iter()
        .find(|file| file.name == name)
        .ok_or_else(|| CompassError::NotFound(format!("workspace artifact '{name}'")))?;
    Ok(file_response(
        w.store.artifact_bytes(&scope, &id, &name).await?,
        &file.media_type,
    ))
}
fn file_response(bytes: Vec<u8>, mime: &str) -> Response {
    let mut response = bytes.into_response();
    if let Ok(mime) = HeaderValue::from_str(mime) {
        response.headers_mut().insert(header::CONTENT_TYPE, mime);
    }
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}
