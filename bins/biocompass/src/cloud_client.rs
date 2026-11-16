//! Stateless `cloud-web` trait adapters delegating sessions, S3 blobs, ACT ledger, catalogs,
//! and WSS `froglet-node` container execution to `compass-hub` over standard HTTP/SSE.

use crate::state::WorkspaceRegistryPort;
use async_trait::async_trait;
use biocompass_agent_core::tools::AgentExecutionPort;
use biocompass_core_traits::{
    BlobStore, CatalogId, CompassError, CompassResult, CreditAmountAct, DatasetGrant, LedgerTx,
    NodeId, OrgId, PaymentGateway, PluginId, PrepareRunRequest, RunDispatcher, RunId, RunLogEvent,
    RunQuote, RunRecord, SessionEvent, SessionEventKind, SessionId, SessionRecord, SessionStore,
};
use biocompass_data_engine::{CatalogManifest, CatalogQuery, CatalogQueryResult};
use biocompass_plugin_spec::{LoadedPlugin, PluginManifest, SkillDoc};
use futures::StreamExt;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::{RwLock, broadcast};

#[derive(Debug, Clone)]
pub struct CompassHubConfig {
    pub base_url: String,
    pub api_key: String,
    pub bundled_plugins_dir: PathBuf,
}

impl CompassHubConfig {
    pub fn from_env(bundled_plugins_dir: PathBuf) -> CompassResult<Self> {
        let base_url = std::env::var("COMPASS_HUB_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8081".into())
            .trim_end_matches('/')
            .to_owned();
        let api_key = std::env::var("COMPASS_HUB_API_KEY")
            .map_err(|_| {
                CompassError::Validation(
                    "COMPASS_HUB_API_KEY must be set when running in --mode cloud-web".into(),
                )
            })?
            .trim()
            .to_owned();
        if api_key.is_empty() {
            return Err(CompassError::Validation(
                "COMPASS_HUB_API_KEY cannot be empty".into(),
            ));
        }
        Ok(Self {
            base_url,
            api_key,
            bundled_plugins_dir,
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct HubWhoAmI {
    pub org_id: OrgId,
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
}

pub struct CompassHubClient {
    http: Client,
    cfg: CompassHubConfig,
    node_id: NodeId,
    provider_id: String,
    requester_id: String,
    local_ui_roots: RwLock<HashMap<PluginId, PathBuf>>,
    log_tx: broadcast::Sender<RunLogEvent>,
}

impl CompassHubClient {
    pub async fn connect(
        cfg: CompassHubConfig,
        bundled_ui_roots: HashMap<PluginId, PathBuf>,
    ) -> CompassResult<(Self, OrgId, String)> {
        let http = Client::builder()
            .build()
            .map_err(|e| CompassError::Internal(format!("build hub HTTP client: {e}")))?;

        let whoami: HubWhoAmI = get_json(&http, &cfg, "/api/v1/auth/whoami").await?;
        let health: HubHealth = get_json(&http, &cfg, "/api/v1/health").await?;
        let (log_tx, _) = broadcast::channel(1_024);

        let engine_label = health.container_engine.clone();
        let client = Self {
            http,
            cfg,
            node_id: health.node_id,
            provider_id: health.provider_id,
            requester_id: health.requester_id,
            local_ui_roots: RwLock::new(bundled_ui_roots),
            log_tx,
        };
        Ok((client, whoami.org_id, engine_label))
    }

    pub async fn fetch_grants(&self) -> CompassResult<Vec<DatasetGrant>> {
        #[derive(Deserialize)]
        struct Resp {
            grants: Vec<DatasetGrant>,
        }
        let r: Resp = get_json(&self.http, &self.cfg, "/api/v1/grants").await?;
        Ok(r.grants)
    }

    fn spawn_log_relay(&self, run_id: RunId) {
        let http = self.http.clone();
        let url = format!(
            "{}/api/v1/runs/{}/events",
            self.cfg.base_url,
            run_id.as_str()
        );
        let api_key = self.cfg.api_key.clone();
        let tx = self.log_tx.clone();
        tokio::spawn(async move {
            let Ok(resp) = http.get(&url).header("X-API-Key", api_key).send().await else {
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
        });
    }
}

async fn map_http_error(resp: reqwest::Response) -> CompassError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| format!("hub HTTP {status}: {body}"));

    match status {
        StatusCode::NOT_FOUND => CompassError::NotFound(msg),
        StatusCode::BAD_REQUEST => CompassError::Validation(msg),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => CompassError::AccessDenied(msg),
        StatusCode::CONFLICT => CompassError::IntegrityViolation(msg),
        StatusCode::PAYMENT_REQUIRED => CompassError::Payment(msg),
        _ => CompassError::Internal(msg),
    }
}

async fn get_json<T: serde::de::DeserializeOwned>(
    http: &Client,
    cfg: &CompassHubConfig,
    path: &str,
) -> CompassResult<T> {
    let resp = http
        .get(format!("{}{}", cfg.base_url, path))
        .header("X-API-Key", &cfg.api_key)
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
    let resp = req
        .send()
        .await
        .map_err(|e| CompassError::Internal(format!("{label} failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(map_http_error(resp).await);
    }
    resp.json::<T>()
        .await
        .map_err(|e| CompassError::Internal(format!("decode {label} JSON: {e}")))
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
                .header("X-API-Key", &self.cfg.api_key)
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
        send_json(
            self.http
                .patch(format!(
                    "{}/api/v1/sessions/{}",
                    self.cfg.base_url,
                    session_id.as_str()
                ))
                .header("X-API-Key", &self.cfg.api_key)
                .json(&json!({ "active_grant_id": active_grant_id })),
            "PATCH /api/v1/sessions/:id",
        )
        .await
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
                .header("X-API-Key", &self.cfg.api_key)
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

    async fn save_run(&self, run: &RunRecord) -> CompassResult<()> {
        let _: Value = send_json(
            self.http
                .post(format!("{}/api/v1/runs", self.cfg.base_url))
                .header("X-API-Key", &self.cfg.api_key)
                .json(run),
            "POST /api/v1/runs",
        )
        .await?;
        Ok(())
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
        let key_clean = key.trim_start_matches('/');
        let r: Resp = send_json(
            self.http
                .put(format!("{}/api/v1/blobs/{}", self.cfg.base_url, key_clean))
                .header("X-API-Key", &self.cfg.api_key)
                .body(bytes.to_vec()),
            "PUT /api/v1/blobs/*key",
        )
        .await?;
        Ok(r.sha256)
    }

    async fn get_bytes(&self, key: &str) -> CompassResult<Vec<u8>> {
        let key_clean = key.trim_start_matches('/');
        let resp = self
            .http
            .get(format!("{}/api/v1/blobs/{}", self.cfg.base_url, key_clean))
            .header("X-API-Key", &self.cfg.api_key)
            .send()
            .await
            .map_err(|e| CompassError::Storage(format!("GET blob '{key}': {e}")))?;
        if !resp.status().is_success() {
            return Err(map_http_error(resp).await);
        }
        let b = resp
            .bytes()
            .await
            .map_err(|e| CompassError::Storage(format!("read blob '{key}': {e}")))?;
        Ok(b.to_vec())
    }

    async fn exists(&self, key: &str) -> CompassResult<bool> {
        let key_clean = key.trim_start_matches('/');
        let resp = self
            .http
            .head(format!("{}/api/v1/blobs/{}", self.cfg.base_url, key_clean))
            .header("X-API-Key", &self.cfg.api_key)
            .send()
            .await
            .map_err(|e| CompassError::Storage(format!("HEAD blob '{key}': {e}")))?;
        Ok(resp.status().is_success())
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
                .header("X-API-Key", &self.cfg.api_key)
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

#[async_trait]
impl RunDispatcher for CompassHubClient {
    async fn prepare_run(&self, req: PrepareRunRequest) -> CompassResult<RunQuote> {
        send_json(
            self.http
                .post(format!("{}/api/v1/runs/prepare", self.cfg.base_url))
                .header("X-API-Key", &self.cfg.api_key)
                .json(&json!({
                    "plugin_id": req.plugin_id,
                    "params": req.params,
                    "grant_id": req.dataset_grant.as_ref().map(|g| g.grant_id.as_str()),
                    "session_id": req.session_id,
                })),
            "POST /api/v1/runs/prepare",
        )
        .await
    }

    async fn confirm_run(&self, run_id: &RunId, idempotency_key: &str) -> CompassResult<RunRecord> {
        self.spawn_log_relay(run_id.clone());
        send_json(
            self.http
                .post(format!(
                    "{}/api/v1/runs/{}/confirm",
                    self.cfg.base_url,
                    run_id.as_str()
                ))
                .header("X-API-Key", &self.cfg.api_key)
                .json(&json!({ "idempotency_key": idempotency_key })),
            "POST /api/v1/runs/:id/confirm",
        )
        .await
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

    async fn list_catalog_manifests(&self) -> Vec<CatalogManifest> {
        #[derive(Deserialize)]
        struct Resp {
            catalogs: Vec<CatalogManifest>,
        }
        get_json::<Resp>(&self.http, &self.cfg, "/api/v1/catalogs")
            .await
            .map(|r| r.catalogs)
            .unwrap_or_default()
    }

    async fn query_catalog(
        &self,
        id: &CatalogId,
        grant: &DatasetGrant,
        query: &CatalogQuery,
    ) -> CompassResult<CatalogQueryResult> {
        send_json(
            self.http
                .post(format!(
                    "{}/api/v1/catalogs/{}/query",
                    self.cfg.base_url,
                    id.as_str()
                ))
                .header("X-API-Key", &self.cfg.api_key)
                .json(&json!({
                    "grant_id": grant.grant_id,
                    "query": query,
                })),
            "POST /api/v1/catalogs/:ns/:name/query",
        )
        .await
    }

    async fn list_plugins(&self) -> Vec<LoadedPlugin> {
        #[derive(Deserialize)]
        struct Resp {
            plugins: Vec<HubPluginDto>,
        }
        let ui_roots = self.local_ui_roots.read().await;
        get_json::<Resp>(&self.http, &self.cfg, "/api/v1/plugins")
            .await
            .map(|r| {
                r.plugins
                    .into_iter()
                    .map(|dto| {
                        let root_dir = ui_roots.get(&dto.manifest.id).cloned();
                        LoadedPlugin {
                            manifest: dto.manifest,
                            skill: dto.skill,
                            root_dir,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn get_plugin(&self, id: &PluginId) -> CompassResult<LoadedPlugin> {
        WorkspaceRegistryPort::list_plugins(self)
            .await
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
                .header("X-API-Key", &self.cfg.api_key)
                .json(&HubPluginDto {
                    manifest: plugin.manifest,
                    skill: plugin.skill,
                }),
            "POST /api/v1/plugins",
        )
        .await?;
        Ok(())
    }

    async fn list_grants(&self, _org_id: &OrgId, _default_cat: &CatalogId) -> Vec<DatasetGrant> {
        self.fetch_grants().await.unwrap_or_default()
    }
}

#[async_trait]
impl AgentExecutionPort for CompassHubClient {
    async fn list_plugins(&self) -> Vec<LoadedPlugin> {
        WorkspaceRegistryPort::list_plugins(self).await
    }

    async fn list_catalog_manifests(&self) -> Vec<CatalogManifest> {
        WorkspaceRegistryPort::list_catalog_manifests(self).await
    }

    async fn query_catalog(
        &self,
        id: &CatalogId,
        grant: &DatasetGrant,
        query: &CatalogQuery,
    ) -> CompassResult<CatalogQueryResult> {
        WorkspaceRegistryPort::query_catalog(self, id, grant, query).await
    }

    async fn prepare_plugin_quote(&self, req: PrepareRunRequest) -> CompassResult<RunQuote> {
        self.prepare_run(req).await
    }
}
