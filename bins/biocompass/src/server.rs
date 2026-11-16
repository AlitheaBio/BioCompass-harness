//! Axum HTTP/SSE API router, strict CSP `/plugin-ui/*` sandbox server, and embedded SPA host.

use crate::state::AppState;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use biocompass_agent_core::AgentStreamEvent;
use biocompass_core_traits::{
    CatalogId, CompassError, CreditAmountAct, PluginId, PrepareRunRequest, RunDispatcher, RunId,
    SessionId,
};
use biocompass_data_engine::CatalogQuery;
use biocompass_node_runtime::verify_froglet_chain_offline;
use biocompass_plugin_spec::{
    DeclarativeViewSpec, LoadedPlugin, PluginManifest, SkillDoc, TemplateKind,
    render_template_files, write_template_to_dir,
};
use futures::StreamExt;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::convert::Infallible;
use std::path::{Component, Path};
use tokio::sync::mpsc;
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};
use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;

#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
struct WebDistAssets;

/// Strict Content-Security-Policy applied to `/plugin-ui/:ns/:slug/*` iframe documents.
/// `connect-src 'none'` blocks direct network egress (`fetch`, `XHR`, `WebSocket`), forcing all
/// communication through `@biocompass/plugin-sdk` `postMessage` JSON-RPC 2.0.
pub const PLUGIN_SANDBOX_CSP: &str = "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'none'; frame-ancestors 'self'";

pub fn build_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/api/v1/health", get(get_health))
        .route("/api/v1/grants", get(list_grants))
        .route("/api/v1/catalogs", get(list_catalogs))
        .route(
            "/api/v1/catalogs/:ns/:name/query",
            post(query_catalog_handler),
        )
        .route("/api/v1/plugins", get(list_plugins_handler))
        .route("/api/v1/plugins/scaffold", post(scaffold_plugin_handler))
        .route("/api/v1/runs", get(list_runs_handler))
        .route("/api/v1/runs/prepare", post(prepare_run_handler))
        .route("/api/v1/runs/:run_id", get(get_run_handler))
        .route("/api/v1/runs/:run_id/confirm", post(confirm_run_handler))
        .route("/api/v1/runs/:run_id/events", get(stream_run_events_handler))
        .route(
            "/api/v1/runs/:run_id/artifacts/*artifact_name",
            get(get_run_artifact_handler),
        )
        .route("/api/v1/froglet/verify", post(verify_froglet_handler))
        .route("/api/v1/ledger", get(get_ledger_handler))
        .route("/api/v1/ledger/topup", post(topup_ledger_handler))
        .route(
            "/api/v1/sessions",
            get(list_sessions_handler).post(create_session_handler),
        )
        .route("/api/v1/sessions/:session_id", patch(update_session_handler))
        .route(
            "/api/v1/sessions/:session_id/events",
            get(list_session_events_handler),
        )
        .route(
            "/api/v1/sessions/:session_id/chat",
            post(stream_session_chat_handler),
        )
        .route(
            "/plugin-ui/:ns/:slug/*asset_path",
            get(serve_plugin_ui_asset),
        )
        .fallback(serve_embedded_spa)
        .layer(cors)
        .with_state(state)
}

struct ApiError(CompassError);

impl From<CompassError> for ApiError {
    fn from(value: CompassError) -> Self {
        Self(value)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            CompassError::NotFound(_) => StatusCode::NOT_FOUND,
            CompassError::Validation(_) => StatusCode::BAD_REQUEST,
            CompassError::AccessDenied(_) => StatusCode::FORBIDDEN,
            CompassError::IntegrityViolation(_) => StatusCode::CONFLICT,
            CompassError::Payment(_) => StatusCode::PAYMENT_REQUIRED,
            CompassError::UpstreamLlm(_) => StatusCode::BAD_GATEWAY,
            CompassError::ContainerEngine(_) | CompassError::Protocol(_) | CompassError::Storage(_) | CompassError::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        (
            status,
            Json(json!({
                "error": self.0.to_string(),
            })),
        )
            .into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

async fn get_health(State(state): State<AppState>) -> Json<Value> {
    let catalogs = state.runtime.list_catalogs().await;
    let plugins = state.runtime.list_plugins().await;
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "mode": state.mode.as_str(),
        "node_id": state.runtime.node_id(),
        "provider_id": state.runtime.provider_id(),
        "requester_id": state.runtime.requester_id(),
        "container_engine": state.container_engine_label,
        "llm_configured": state.agent.is_llm_configured(),
        "llm_model": state.agent.llm_model_name(),
        "catalog_count": catalogs.len(),
        "plugin_count": plugins.len(),
    }))
}

async fn list_grants(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "grants": state.available_grants(),
    }))
}

async fn list_catalogs(State(state): State<AppState>) -> Json<Value> {
    let items: Vec<Value> = state
        .runtime
        .list_catalogs()
        .await
        .into_iter()
        .map(|c| serde_json::to_value(c.manifest()).unwrap_or(Value::Null))
        .collect();
    Json(json!({ "catalogs": items }))
}

#[derive(Debug, Deserialize)]
struct CatalogQueryRequest {
    grant_id: Option<String>,
    #[serde(default)]
    query: CatalogQuery,
}

async fn query_catalog_handler(
    State(state): State<AppState>,
    AxumPath((ns, name)): AxumPath<(String, String)>,
    Json(req): Json<CatalogQueryRequest>,
) -> ApiResult<Json<Value>> {
    let catalog_id =
        CatalogId::new(format!("{ns}/{name}")).map_err(CompassError::Validation)?;
    let catalog = state.runtime.get_catalog(&catalog_id).await?;
    let grant = state
        .resolve_grant(req.grant_id.as_deref().or(Some("full")))?
        .ok_or_else(|| CompassError::AccessDenied("a DatasetGrant is required".into()))?;
    if grant.catalog_id != catalog_id {
        return Err(CompassError::AccessDenied(format!(
            "grant '{}' does not cover catalog '{}'",
            grant.grant_id, catalog_id
        ))
        .into());
    }
    let result = catalog.query(&grant, &req.query)?;
    Ok(Json(serde_json::to_value(result).unwrap_or(Value::Null)))
}

#[derive(Debug, Serialize)]
struct PluginSummaryDto {
    manifest: PluginManifest,
    skill: Option<SkillDoc>,
    views: Vec<DeclarativeViewSpec>,
    has_custom_ui: bool,
    ui_entry_url: Option<String>,
}

fn to_plugin_dto(plugin: LoadedPlugin) -> PluginSummaryDto {
    let has_ui_bundle = plugin
        .manifest
        .ui
        .as_ref()
        .zip(plugin.root_dir.as_ref())
        .is_some_and(|(ui, root)| root.join(&ui.entry).is_file());
    let ui_entry_url = if has_ui_bundle {
        Some(format!("/plugin-ui/{}/index.html", plugin.manifest.id))
    } else {
        None
    };
    let views = plugin.manifest.views.clone();
    PluginSummaryDto {
        manifest: plugin.manifest,
        skill: plugin.skill,
        views,
        has_custom_ui: has_ui_bundle,
        ui_entry_url,
    }
}

async fn list_plugins_handler(State(state): State<AppState>) -> Json<Value> {
    let dtos: Vec<PluginSummaryDto> = state
        .runtime
        .list_plugins()
        .await
        .into_iter()
        .map(to_plugin_dto)
        .collect();
    Json(json!({ "plugins": dtos }))
}

#[derive(Debug, Deserialize)]
struct ScaffoldPluginRequest {
    template: String,
    plugin_id: String,
    display_name: String,
    #[serde(default)]
    include_vite_ui: bool,
}

async fn scaffold_plugin_handler(
    State(state): State<AppState>,
    Json(req): Json<ScaffoldPluginRequest>,
) -> ApiResult<Json<Value>> {
    let kind = TemplateKind::parse(&req.template)?;
    let files = render_template_files(
        kind,
        &req.plugin_id,
        &req.display_name,
        req.include_vite_ui,
    )?;

    let slug = req
        .plugin_id
        .split('/')
        .nth(1)
        .filter(|s| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .ok_or_else(|| {
            CompassError::Validation("plugin_id must be '<org>/<alphanumeric-slug>'".into())
        })?;

    let target_dir = state.user_plugins_dir.join(slug);
    write_template_to_dir(
        kind,
        &req.plugin_id,
        &req.display_name,
        req.include_vite_ui,
        &target_dir,
    )?;

    let registered = if state.allow_local_dev_tags {
        if let Ok(loaded) = LoadedPlugin::load_from_dir(&target_dir, true) {
            state.runtime.register_plugin(loaded).await.is_ok()
        } else {
            false
        }
    } else {
        false
    };

    Ok(Json(json!({
        "plugin_id": req.plugin_id,
        "template": kind.as_str(),
        "directory": target_dir,
        "registered": registered,
        "files": files,
    })))
}

#[derive(Debug, Deserialize)]
struct ApiPrepareRunRequest {
    plugin_id: String,
    #[serde(default = "empty_json_obj")]
    params: Value,
    grant_id: Option<String>,
    session_id: Option<String>,
}

fn empty_json_obj() -> Value {
    json!({})
}

async fn prepare_run_handler(
    State(state): State<AppState>,
    Json(req): Json<ApiPrepareRunRequest>,
) -> ApiResult<Json<Value>> {
    let plugin_id = PluginId::new(req.plugin_id).map_err(CompassError::Validation)?;
    let session_id = req
        .session_id
        .map(SessionId::new)
        .transpose()
        .map_err(CompassError::Validation)?;
    let grant = state.resolve_grant(req.grant_id.as_deref())?;

    let quote = state
        .runtime
        .prepare_run(PrepareRunRequest {
            org_id: state.default_org_id.clone(),
            session_id,
            plugin_id,
            params: req.params,
            dataset_grant: grant,
        })
        .await?;
    Ok(Json(serde_json::to_value(quote).unwrap_or(Value::Null)))
}

#[derive(Debug, Deserialize, Default)]
struct ApiConfirmRunRequest {
    idempotency_key: Option<String>,
}

async fn confirm_run_handler(
    State(state): State<AppState>,
    AxumPath(run_id_raw): AxumPath<String>,
    body: Option<Json<ApiConfirmRunRequest>>,
) -> ApiResult<Json<Value>> {
    let run_id = RunId::new(run_id_raw).map_err(CompassError::Validation)?;
    let idem = body
        .and_then(|b| b.0.idempotency_key)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("idem-{}", Uuid::new_v4().simple()));

    let record = state.runtime.confirm_run(&run_id, &idem).await?;
    Ok(Json(serde_json::to_value(record).unwrap_or(Value::Null)))
}

async fn list_runs_handler(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let runs = state.session_store.list_runs(&state.default_org_id).await?;
    Ok(Json(json!({ "runs": runs })))
}

async fn get_run_handler(
    State(state): State<AppState>,
    AxumPath(run_id_raw): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let run_id = RunId::new(run_id_raw).map_err(CompassError::Validation)?;
    let run = state.session_store.get_run(&run_id).await?;
    Ok(Json(serde_json::to_value(run).unwrap_or(Value::Null)))
}

async fn stream_run_events_handler(
    State(state): State<AppState>,
    AxumPath(run_id_raw): AxumPath<String>,
) -> ApiResult<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>> {
    let target_run_id = RunId::new(run_id_raw).map_err(CompassError::Validation)?;
    let rx = state.runtime.subscribe_logs();
    let stream = BroadcastStream::new(rx).filter_map(move |msg| {
        let target = target_run_id.clone();
        async move {
            match msg {
                Ok(ev) if ev.run_id == target => {
                    let payload = serde_json::to_string(&ev).unwrap_or_default();
                    Some(Ok(Event::default().event("log").data(payload)))
                }
                _ => None,
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn get_run_artifact_handler(
    State(state): State<AppState>,
    AxumPath((run_id_raw, artifact_name)): AxumPath<(String, String)>,
) -> ApiResult<Response> {
    let run_id = RunId::new(run_id_raw).map_err(CompassError::Validation)?;
    let clean_name = artifact_name.trim_start_matches('/');
    let run = state.session_store.get_run(&run_id).await?;
    let art = run
        .artifacts
        .iter()
        .find(|a| a.name == clean_name)
        .ok_or_else(|| {
            CompassError::NotFound(format!("artifact '{clean_name}' not found on run '{run_id}'"))
        })?;
    let bytes = state.blob_store.get_bytes(&art.blob_key).await?;
    let mut resp = (StatusCode::OK, bytes).into_response();
    if let Ok(ct) = HeaderValue::from_str(&art.media_type) {
        resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    resp.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(resp)
}

#[derive(Debug, Deserialize)]
struct VerifyFrogletRequest {
    documents: Vec<Value>,
    now: Option<i64>,
}

async fn verify_froglet_handler(Json(req): Json<VerifyFrogletRequest>) -> Json<Value> {
    let report = verify_froglet_chain_offline(&req.documents, req.now);
    Json(serde_json::to_value(report).unwrap_or(Value::Null))
}

async fn get_ledger_handler(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let bal = state
        .payment_gateway
        .available_balance(&state.default_org_id)
        .await?;
    let txs = state
        .payment_gateway
        .list_transactions(&state.default_org_id)
        .await?;
    Ok(Json(json!({
        "org_id": state.default_org_id,
        "available_milli_act": bal.milli_act(),
        "available_display": bal.display_act(),
        "transactions": txs,
    })))
}

#[derive(Debug, Deserialize)]
struct TopUpRequest {
    amount_milli_act: i64,
    reference: String,
}

async fn topup_ledger_handler(
    State(state): State<AppState>,
    Json(req): Json<TopUpRequest>,
) -> ApiResult<Json<Value>> {
    let tx = state
        .payment_gateway
        .credit_topup(
            &state.default_org_id,
            CreditAmountAct::from_milli_act(req.amount_milli_act),
            &req.reference,
            false,
        )
        .await?;
    Ok(Json(serde_json::to_value(tx).unwrap_or(Value::Null)))
}

async fn list_sessions_handler(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let sessions = state
        .session_store
        .list_sessions(&state.default_org_id)
        .await?;
    Ok(Json(json!({ "sessions": sessions })))
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    title: String,
    active_grant_id: Option<String>,
}

async fn create_session_handler(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> ApiResult<Json<Value>> {
    let grant = state
        .resolve_grant(req.active_grant_id.as_deref().or(Some("full")))?
        .ok_or_else(|| CompassError::Validation("invalid default grant".into()))?;
    let session = state
        .session_store
        .create_session(&state.default_org_id, &req.title, &grant.grant_id)
        .await?;
    Ok(Json(serde_json::to_value(session).unwrap_or(Value::Null)))
}

#[derive(Debug, Deserialize)]
struct UpdateSessionRequest {
    active_grant_id: String,
}

async fn update_session_handler(
    State(state): State<AppState>,
    AxumPath(session_id_raw): AxumPath<String>,
    Json(req): Json<UpdateSessionRequest>,
) -> ApiResult<Json<Value>> {
    let session_id = SessionId::new(session_id_raw).map_err(CompassError::Validation)?;
    let _ = state.resolve_grant(Some(&req.active_grant_id))?;
    let updated = state
        .session_store
        .update_session_grant(&session_id, &req.active_grant_id)
        .await?;
    Ok(Json(serde_json::to_value(updated).unwrap_or(Value::Null)))
}

async fn list_session_events_handler(
    State(state): State<AppState>,
    AxumPath(session_id_raw): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let session_id = SessionId::new(session_id_raw).map_err(CompassError::Validation)?;
    let events = state.session_store.list_events(&session_id).await?;
    Ok(Json(json!({ "events": events })))
}

#[derive(Debug, Deserialize)]
struct ChatTurnRequest {
    message: String,
}

async fn stream_session_chat_handler(
    State(state): State<AppState>,
    AxumPath(session_id_raw): AxumPath<String>,
    Json(req): Json<ChatTurnRequest>,
) -> ApiResult<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>> {
    let session_id = SessionId::new(session_id_raw).map_err(CompassError::Validation)?;
    let session = state.session_store.get_session(&session_id).await?;
    let active_grant = state.resolve_grant(Some(&session.active_grant_id))?;

    let (tx, rx) = mpsc::channel::<AgentStreamEvent>(128);
    let agent = state.agent.clone();
    let msg = req.message;
    tokio::spawn(async move {
        if let Err(err) = agent
            .run_turn(&session_id, &msg, active_grant.as_ref(), Some(tx.clone()))
            .await
        {
            let _ = tx.send(AgentStreamEvent::Error(err.to_string())).await;
        }
    });

    let stream = ReceiverStream::new(rx).map(|ev| {
        let data = serde_json::to_string(&ev).unwrap_or_default();
        Ok(Event::default().event("agent").data(data))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn serve_plugin_ui_asset(
    State(state): State<AppState>,
    AxumPath((ns, slug, asset_path)): AxumPath<(String, String, String)>,
) -> ApiResult<Response> {
    let plugin_id = PluginId::new(format!("{ns}/{slug}")).map_err(CompassError::Validation)?;
    let plugin = state.runtime.get_plugin(&plugin_id).await?;
    let ui_spec = plugin.manifest.ui.as_ref().ok_or_else(|| {
        CompassError::NotFound(format!("plugin '{plugin_id}' has no custom UI declared"))
    })?;
    let root_dir = plugin.root_dir.as_ref().ok_or_else(|| {
        CompassError::NotFound(format!("plugin '{plugin_id}' has no on-disk bundle directory"))
    })?;

    // Resolve UI dist directory from `ui.entry` (e.g. `ui/dist/index.html` -> `ui/dist`).
    let entry_rel = Path::new(&ui_spec.entry);
    let ui_dist_dir = entry_rel
        .parent()
        .map(|p| root_dir.join(p))
        .unwrap_or_else(|| root_dir.to_path_buf());

    let requested = asset_path.trim_start_matches('/');
    let rel = if requested.is_empty() {
        "index.html"
    } else {
        requested
    };

    let mut target = ui_dist_dir.clone();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(seg) => target.push(seg),
            _ => {
                return Err(
                    CompassError::AccessDenied("unsafe path traversal in plugin UI asset".into())
                        .into(),
                );
            }
        }
    }

    if !target.is_file() {
        return Err(
            CompassError::NotFound(format!("plugin UI asset '{rel}' not found")).into(),
        );
    }

    let bytes = std::fs::read(&target)
        .map_err(|e| CompassError::Storage(format!("read plugin UI asset: {e}")))?;
    let mime = mime_guess::from_path(&target).first_or_octet_stream();

    let mut resp = (StatusCode::OK, bytes).into_response();
    if let Ok(ct) = HeaderValue::from_str(mime.as_ref()) {
        resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    resp.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PLUGIN_SANDBOX_CSP),
    );
    resp.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(resp)
}

async fn serve_embedded_spa(uri: Uri) -> Response {
    let raw_path = uri.path().trim_start_matches('/');
    let path = if raw_path.is_empty() {
        "index.html"
    } else {
        raw_path
    };

    if let Some(file) = WebDistAssets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        let mut resp = (StatusCode::OK, file.data.into_owned()).into_response();
        if let Ok(ct) = HeaderValue::from_str(mime.as_ref()) {
            resp.headers_mut().insert(header::CONTENT_TYPE, ct);
        }
        resp.headers_mut().insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
        return resp;
    }

    if !raw_path.starts_with("api/")
        && let Some(index) = WebDistAssets::get("index.html")
    {
        let mut resp = (StatusCode::OK, index.data.into_owned()).into_response();
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        resp.headers_mut().insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
        return resp;
    }

    (StatusCode::NOT_FOUND, "not found").into_response()
}
