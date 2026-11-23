//! Axum HTTP/SSE API router, strict CSP `/plugin-ui/*` sandbox server, and embedded SPA host.

use crate::cloud_client::agent_budget::{BudgetedAgentPort, ExecutionBudget};
use crate::cloud_client::{CALLER_AUTH, CallerAuth, CompassHubClient, current_caller_auth};
use crate::llm_backends::{
    LlmConfigResponse, UpdateLlmConfigRequest, discover_local_llm_backends,
    normalize_llm_config_request, redact_llm_config, resolve_provider_controls,
    test_llm_connection,
};
use crate::state::{AppState, DeploymentMode, local_catalog_dir, save_saved_llm_config};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as AxumPath, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, delete, get, patch, post, put};
use axum::{Json, Router};
use biocompass_agent_core::AgentStreamEvent;
use biocompass_core_traits::{
    CatalogId, CompassError, CreditAmountAct, GrantLevel, IngestFormat, IngestMode, PluginId,
    PrepareRunRequest, ProjectId, QueryAst, RunId, SavedViewPlane, SavedViewRecord,
    SavedViewSourcePin, SavedViewStatus, SavedViewVerificationReport, SessionId,
};
use biocompass_data_engine::{
    DuckDbCatalogEngine, QuarantineIngestSpec, validate_and_normalize_quarantine_dir,
};
use biocompass_node_runtime::verify_froglet_chain_offline;
use biocompass_plugin_spec::{
    DeclarativeViewSpec, LoadedPlugin, PluginExecutionPlane, PluginManifest, ResolvedPluginOffer,
    SkillDoc, TemplateKind, render_template_files, write_template_to_dir,
};
use futures::StreamExt;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::convert::Infallible;
use std::path::{Component, Path};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::CorsLayer;
use uuid::Uuid;

#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
struct WebDistAssets;

/// Strict Content-Security-Policy applied to `/plugin-ui/:ns/:slug/*` iframe documents.
/// `connect-src 'none'` blocks direct network egress (`fetch`, `XHR`, `WebSocket`), forcing all
/// communication through `@biocompass/plugin-sdk` `postMessage` JSON-RPC 2.0.
pub const PLUGIN_SANDBOX_CSP: &str = "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'none'; frame-ancestors 'self'";

async fn caller_auth_middleware(req: Request, next: Next) -> Response {
    let hdrs = req.headers();
    let auth = CallerAuth {
        cookie: hdrs
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        authorization: hdrs
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        api_key: hdrs
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    };
    CALLER_AUTH.scope(auth, next.run(req)).await
}

pub fn build_router(state: AppState) -> Router {
    let cors = CorsLayer::permissive();
    let prepare = post(prepare_run_handler);
    let (prepare, run_events) = if let Some(hub) = &state.hub_client {
        (
            prepare.layer(middleware::from_fn_with_state(hub.clone(), limit_hosted_prepare)),
            get(proxy_cloud_route_handler),
        )
    } else {
        (prepare, get(stream_run_events_handler))
    };
    let release_routes = if state.hub_client.is_some() {
        Router::new()
        .route("/api/v1/catalogs/:ns/:name/changes", get(proxy_cloud_route_handler))
        .route("/api/v1/catalogs/:ns/:name/changes/preview", post(proxy_cloud_route_handler))
        .route("/api/v1/catalogs/:ns/:name/changes/:change_id/publish", post(proxy_cloud_route_handler))
    } else {
        use crate::native_import::operator::*;
        Router::new()
            .route("/api/v1/catalogs/:ns/:name/changes", get(list_changes))
            .route("/api/v1/catalogs/:ns/:name/changes/preview", post(preview_change))
            .route("/api/v1/catalogs/:ns/:name/changes/:change_id/publish", post(publish_change))
    };

    Router::new()
        .merge(crate::workspace::routes())
        .route("/api/v1/health", get(get_health))
        .route("/api/v1/app-distribution", get(crate::app_distribution::discover))
        .route("/api/v1/app-distribution/download", get(crate::app_distribution::download))
        .route("/api/v1/auth/whoami", get(get_whoami_handler))
        .route("/api/v1/auth/*path", any(proxy_cloud_route_handler))
        .route("/api/v1/orgs", any(proxy_cloud_route_handler))
        .route("/api/v1/orgs/*path", any(proxy_cloud_route_handler))
        .route("/api/v1/audit-logs", get(proxy_cloud_route_handler))
        .route("/api/v1/billing/*path", any(proxy_cloud_route_handler))
        .route("/api/v1/admin/*path", any(proxy_cloud_route_handler))
        .route("/api/v1/nodes", get(proxy_cloud_route_handler))
        .route("/api/v1/nodes/*path", any(proxy_cloud_route_handler))
        .route(
            "/api/v1/llm/config",
            get(get_llm_config_handler).put(update_llm_config_handler),
        )
        .route("/api/v1/llm/test", post(test_llm_connection_handler))
        .route(
            "/api/v1/grants",
            get(list_grants).post(proxy_cloud_route_handler),
        )
        .route("/api/v1/grants/*path", any(proxy_cloud_route_handler))
        .route("/api/v1/local/importers", get(crate::native_import::discover))
        .route("/api/v1/local/importers/:ns/:slug/import", post(crate::native_import::import))
        .route("/api/v1/catalogs", get(list_catalogs))
        .route("/api/v1/catalogs/seed-demo", post(proxy_cloud_route_handler))
        .route(
            "/api/v1/catalogs/import-local",
            post(import_local_catalog_handler).layer(DefaultBodyLimit::max(256 * 1024 * 1024)),
        )
        .route(
            "/api/v1/saved-views",
            get(list_saved_views_handler).post(create_saved_view_handler),
        )
        .route(
            "/api/v1/saved-views/:view_id",
            delete(delete_saved_view_handler),
        )
        .route(
            "/api/v1/saved-views/:view_id/verify",
            post(verify_saved_view_handler),
        )
        .route("/api/v1/uploads/quarantine", get(proxy_cloud_route_handler))
        .route(
            "/api/v1/uploads/quarantine/*path",
            any(proxy_cloud_route_handler),
        )
        .route(
            "/api/v1/dataset-offers",
            get(proxy_cloud_route_handler).post(proxy_cloud_route_handler),
        )
        .route(
            "/api/v1/dataset-offers/*path",
            any(proxy_cloud_route_handler),
        )
        .route(
            "/api/v1/module-offers",
            get(proxy_cloud_route_handler).post(proxy_cloud_route_handler),
        )
        .route("/api/v1/module-offers/*path", any(proxy_cloud_route_handler))
        .route(
            "/api/v1/result-shares",
            get(proxy_cloud_route_handler).post(proxy_cloud_route_handler),
        )
        .route("/api/v1/result-shares/:id", delete(proxy_cloud_route_handler))
        .route("/api/v1/result-shares/:id/download", get(proxy_cloud_route_handler))
        .route(
            "/api/v1/workbench/module-offers/:id/run-module",
            get(offered_run_module_handler),
        )
        .route(
            "/api/v1/catalogs/:ns/:name/query",
            post(read_catalog_handler),
        )
        .route(
            "/api/v1/catalogs/:ns/:name/export",
            post(read_catalog_handler),
        )
        .merge(release_routes)
        .route("/api/v1/local/owner-session", post(crate::native_import::operator::redeem))
        .route(
            "/api/v1/catalogs/:ns/:name/rollback",
            post(rollback_catalog_handler),
        )
        .route(
            "/api/v1/catalogs/:ns/:name/mutate",
            post(proxy_cloud_route_handler),
        )
        .route(
            "/api/v1/catalogs/:ns/:name/expire-snapshots",
            post(proxy_cloud_route_handler),
        )
        .route(
            "/api/v1/catalogs/:ns/:name/snapshots/:snapshot_id/pin",
            post(pin_catalog_snapshot_handler),
        )
        .route(
            "/api/v1/plugins",
            get(list_plugins_handler).post(proxy_cloud_route_handler),
        )
        .route("/api/v1/plugins/versions", get(proxy_cloud_route_handler))
        .route("/api/v1/plugins/downloads", get(proxy_cloud_route_handler))
        .route("/api/v1/plugins/:ns/:slug/versions/:version/package", get(proxy_cloud_route_handler))
        .route("/api/v1/plugins/:ns/:slug/versions/:version/download-policy",
            get(proxy_cloud_route_handler).put(proxy_cloud_route_handler))
        .route("/api/v1/plugins/:ns/:slug/versions/:version/image", get(proxy_cloud_route_handler))
        .route(
            "/api/v1/plugins/:ns/:slug/versions/:version/ui-bundle",
            put(proxy_cloud_route_handler),
        )
        .route("/api/v1/plugins/:ns/:slug/install", post(install_plugin_handler))
        .route("/api/v1/plugins/:ns/:slug/versions/:version/install", axum::routing::delete(remove_plugin_handler))
        .route("/api/v1/plugins/scaffold", post(scaffold_plugin_handler))
        .route("/api/v1/plugins/ides", get(list_installed_ides_handler))
        .route("/api/v1/plugins/open-ide", post(open_plugin_ide_handler))
        .route("/api/v1/runs", get(list_runs_handler))
        .route("/api/v1/runs/prepare", prepare)
        .route("/api/v1/runs/:run_id", get(get_run_handler))
        .route("/api/v1/runs/:run_id/confirm", post(confirm_run_handler))
        .route("/api/v1/runs/:run_id/cancel", post(proxy_cloud_route_handler))
        .route("/api/v1/runs/:run_id/events", run_events)
        .route("/api/v1/runs/:run_id/chain", get(get_run_chain_handler))
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
        .route(
            "/api/v1/sessions/:session_id",
            get(get_session_handler)
                .patch(update_session_handler)
                .delete(delete_session_handler),
        )
        .route(
            "/api/v1/projects",
            get(list_projects_handler).post(create_project_handler),
        )
        .route(
            "/api/v1/projects/:project_id",
            patch(update_project_handler).delete(delete_project_handler),
        )
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
        .route("/plugin-ui-version/:ns/:slug/:version/*asset_path", get(serve_pinned_plugin_ui))
        .route("/plugin-ui-access/:ticket/*asset_path", get(proxy_cloud_route_handler))
        .route("/plugin-icon/:ns/:slug", get(serve_plugin_icon))
        .fallback(serve_embedded_spa)
        .layer(middleware::from_fn(caller_auth_middleware))
        .layer(cors)
        .with_state(state)
}

pub(crate) struct ApiError(CompassError);

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
            CompassError::AccessDenied(msg) => {
                if msg.starts_with("UNAUTHENTICATED:") {
                    StatusCode::UNAUTHORIZED
                } else {
                    StatusCode::FORBIDDEN
                }
            }
            CompassError::IntegrityViolation(_) => StatusCode::CONFLICT,
            CompassError::Payment(_) => StatusCode::PAYMENT_REQUIRED,
            CompassError::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
            CompassError::UpstreamLlm(_) => StatusCode::BAD_GATEWAY,
            CompassError::ContainerEngine(_)
            | CompassError::Protocol(_)
            | CompassError::Storage(_)
            | CompassError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let mut body = serde_json::Map::new();
        body.insert("error".into(), json!(self.0.user_message()));
        body.insert("code".into(), json!(self.0.code()));
        if let Some(hint) = self.0.hint() {
            body.insert("hint".into(), json!(hint));
        }
        (status, Json(Value::Object(body))).into_response()
    }
}

pub(crate) type ApiResult<T> = Result<T, ApiError>;

async fn get_health(State(state): State<AppState>) -> Json<Value> {
    let catalogs = state
        .registry
        .list_catalog_manifests()
        .await
        .unwrap_or_default();
    let plugins = state.registry.list_plugins().await.unwrap_or_default();
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "mode": state.mode.as_str(),
        "node_id": state.registry.node_id(),
        "provider_id": state.registry.provider_id(),
        "requester_id": state.registry.requester_id(),
        "container_engine": state.container_engine_label,
        "llm_configured": state.agent.is_llm_configured().await,
        "llm_model": state.agent.llm_model_name().await,
        "catalog_count": catalogs.len(),
        "plugin_count": plugins.len(),
        "workspace_hub_url": state.installed_workspace.as_deref().map(|w| w.hub_url()),
    }))
}

async fn get_whoami_handler(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if let Some(hub) = &state.hub_client {
        let path_and_query = uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/api/v1/auth/whoami");
        return Ok(hub
            .proxy_raw(reqwest::Method::GET, path_and_query, &headers, Vec::new())
            .await?);
    }
    Ok(Json(json!({
        "mode": "local",
        "org_id": state.default_org_id,
        "actor": "local-user",
        "role": "owner",
        "is_platform_admin": true,
    }))
    .into_response())
}

async fn proxy_cloud_route_handler(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let Some(hub) = &state.hub_client else {
        return Err(CompassError::AccessDenied(
            "this endpoint is only available in cloud-web mode".into(),
        )
        .into());
    };
    let req_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|e| CompassError::Validation(format!("invalid HTTP method: {e}")))?;
    let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    // Never forward internal worker artifact upload endpoints from cloud-web.
    if path_and_query.starts_with("/api/v1/nodes/runs/") {
        return Err(
            CompassError::AccessDenied("FORBIDDEN: worker-only endpoint".into()).into(),
        );
    }
    Ok(hub
        .proxy_raw(req_method, path_and_query, &headers, body.to_vec())
        .await?)
}

async fn list_grants(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    if let Some(hub) = &state.hub_client {
        return Ok(hub
            .proxy_raw(reqwest::Method::GET, "/api/v1/grants", &headers, Vec::new())
            .await?);
    }
    let grants = state.available_grants().await?;
    Ok(Json(json!({ "grants": grants })).into_response())
}

async fn list_catalogs(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    if let Some(hub) = &state.hub_client {
        return Ok(hub.proxy_raw(reqwest::Method::GET, "/api/v1/catalogs", &headers, Vec::new()).await?);
    }
    let manifests = state.registry.list_catalog_manifests().await?;
    let node = state.registry.node_id();
    let items: Vec<_> = manifests.into_iter().map(|manifest| {
        let mut item = json!(manifest);
        item["location"] = json!({"kind": "workspace", "node_id": node});
        item
    }).collect();
    Ok(Json(json!({ "catalogs": items })).into_response())
}

#[derive(Debug, Deserialize)]
struct CatalogQueryRequest {
    grant_id: Option<String>,
    #[serde(default)]
    query: QueryAst,
}

async fn read_catalog_handler(
    State(state): State<AppState>,
    AxumPath((ns, name)): AxumPath<(String, String)>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    if let Some(hub) = &state.hub_client {
        let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
        let mut upstream = body.to_vec();
        if uri.path().ends_with("/query") {
            let mut payload: Value = serde_json::from_slice(&body)
                .map_err(|e| CompassError::Validation(format!("invalid catalog query JSON: {e}")))?;
            let requested = payload.get("grant_id").and_then(Value::as_str).unwrap_or("full");
            if ["full", "partner", "compute_only", "none", ""].iter()
                .any(|alias| requested.trim().eq_ignore_ascii_case(alias))
            {
                let grant = state.resolve_grant(Some(requested)).await?
                    .ok_or_else(|| CompassError::AccessDenied("a DatasetGrant is required".into()))?;
                let fields = payload.as_object_mut()
                    .ok_or_else(|| CompassError::Validation("expected a catalog query object".into()))?;
                fields.insert("grant_id".into(), json!(grant.grant_id));
                upstream = serde_json::to_vec(&payload)
                    .map_err(|e| CompassError::Validation(format!("encode catalog query: {e}")))?;
            }
        }
        return Ok(hub
            .proxy_raw(reqwest::Method::POST, path_and_query, &headers, upstream)
            .await?);
    }
    let req: CatalogQueryRequest = serde_json::from_slice(&body)
        .map_err(|e| CompassError::Validation(format!("invalid catalog query JSON: {e}")))?;
    let catalog_id =
        CatalogId::new(format!("{ns}/{name}")).map_err(CompassError::Validation)?;
    let grant = state
        .resolve_grant(req.grant_id.as_deref().or(Some("full")))
        .await?
        .ok_or_else(|| CompassError::AccessDenied("a DatasetGrant is required".into()))?;
    if grant.catalog_id != catalog_id {
        return Err(CompassError::AccessDenied(format!(
            "grant '{}' does not cover catalog '{}'",
            grant.grant_id, catalog_id
        ))
        .into());
    }
    if uri.path().ends_with("/export") && (grant.level != GrantLevel::Rows || !grant.allow_local_export) {
        return Err(CompassError::AccessDenied(format!(
            "grant '{}' does not permit local slice export for catalog '{}'",
            grant.grant_id, catalog_id
        ))
        .into());
    }
    let result = state
        .registry
        .query_catalog(&catalog_id, &grant, &req.query)
        .await?;
    Ok(Json(serde_json::to_value(result).unwrap_or(Value::Null)).into_response())
}

#[derive(Debug, Deserialize)]
struct RollbackCatalogRequest {
    target_snapshot_id: u64,
}

async fn rollback_catalog_handler(
    State(state): State<AppState>,
    AxumPath((ns, name)): AxumPath<(String, String)>,
    Json(req): Json<RollbackCatalogRequest>,
) -> ApiResult<Json<Value>> {
    let catalog_id =
        CatalogId::new(format!("{ns}/{name}")).map_err(CompassError::Validation)?;
    catalog_id.ensure_legacy_writable()?;
    let summary = state
        .registry
        .rollback_catalog_snapshot(&catalog_id, req.target_snapshot_id)
        .await?;
    Ok(Json(json!({ "catalog": summary })))
}

#[derive(Debug, Deserialize)]
struct PinSnapshotRequest {
    #[serde(default = "default_true")]
    pinned: bool,
}

const fn default_true() -> bool {
    true
}

async fn pin_catalog_snapshot_handler(
    State(state): State<AppState>,
    AxumPath((ns, name, snapshot_id)): AxumPath<(String, String, u64)>,
    Json(req): Json<PinSnapshotRequest>,
) -> ApiResult<Json<Value>> {
    let catalog_id =
        CatalogId::new(format!("{ns}/{name}")).map_err(CompassError::Validation)?;
    catalog_id.ensure_legacy_writable()?;
    let summary = state
        .registry
        .set_catalog_snapshot_pin(&catalog_id, snapshot_id, req.pinned)
        .await?;
    Ok(Json(json!({ "catalog": summary })))
}

#[derive(Debug, Serialize)]
struct PluginSummaryDto {
    manifest: PluginManifest,
    skill: Option<SkillDoc>,
    views: Vec<DeclarativeViewSpec>,
    effective_offers: Vec<ResolvedPluginOffer>,
    has_custom_ui: bool,
    ui_entry_url: Option<String>,
    author: String,
    icon_url: Option<String>,
    execution_target: String,
    execution_label: String,
    requires_catalog_level: GrantLevel,
    removable: bool,
    removable_versions: Vec<String>,
    detached_versions: Vec<String>,
    installed: bool,
}

fn resolve_plugin_author(manifest: &PluginManifest) -> String {
    if let Some(author) = manifest.author.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return author.to_owned();
    }
    let ns = manifest.id.as_str().split('/').next().unwrap_or("");
    if ns.eq_ignore_ascii_case("alithea") {
        "Alithea Bio".into()
    } else if ns.is_empty() {
        "Community".into()
    } else {
        ns.to_owned()
    }
}

fn resolve_execution_target(state: &AppState, manifest: &PluginManifest) -> (&'static str, &'static str) {
    match manifest.execution {
        PluginExecutionPlane::Local => return ("local", "Local · This Laptop"),
        PluginExecutionPlane::CloudOnly => {
            let provider = state.registry.provider_id().to_ascii_lowercase();
            let node = state.registry.node_id().as_str().to_ascii_lowercase();
            if provider.contains("alithea") || node.contains("alithea") || state.mode == DeploymentMode::Local {
                return ("alithea_cloud", "Alithea Bio Cloud");
            }
            return ("froglet_node", "Froglet Node");
        }
        PluginExecutionPlane::Either => {}
    }
    match state.mode {
        DeploymentMode::Local => ("local", "Local · This Laptop"),
        DeploymentMode::CloudWeb => {
            let provider = state.registry.provider_id().to_ascii_lowercase();
            let node = state.registry.node_id().as_str().to_ascii_lowercase();
            if provider.contains("alithea") || node.contains("alithea") {
                ("alithea_cloud", "Alithea Bio Cloud")
            } else {
                ("froglet_node", "Froglet Node")
            }
        }
    }
}

fn resolve_safe_plugin_icon(plugin: &LoadedPlugin) -> Option<std::path::PathBuf> {
    let icon_rel = plugin.manifest.icon.as_deref()?.trim();
    if icon_rel.is_empty() {
        return None;
    }
    let root = plugin.root_dir.as_ref()?;
    let mut target = root.clone();
    for comp in Path::new(icon_rel).components() {
        match comp {
            Component::Normal(seg) => target.push(seg),
            _ => return None,
        }
    }
    target.is_file().then_some(target)
}

async fn to_plugin_dto(state: &AppState, plugin: LoadedPlugin, published_ui: Option<bool>) -> PluginSummaryDto {
    let has_ui_bundle = if let Some(ready) = published_ui {
        ready && plugin.manifest.ui.is_some()
    } else if let Some((ui, root)) = plugin.manifest.ui.as_ref().zip(plugin.root_dir.as_ref()) {
        root.join(&ui.entry).is_file()
    } else {
        // Older hubs sent only a manifest.
        state.mode == DeploymentMode::CloudWeb && plugin.manifest.ui.is_some()
    };
    let ui_entry_url = plugin
        .manifest
        .ui
        .as_ref()
        .filter(|_| has_ui_bundle)
        .and_then(|ui| Path::new(&ui.entry).file_name()?.to_str())
        .map(|entry| pinned_ui_path(&plugin.manifest.id, &plugin.manifest.version, entry));
    let icon_url = if resolve_safe_plugin_icon(&plugin).is_some() {
        Some(format!("/plugin-icon/{}", plugin.manifest.id))
    } else {
        None
    };
    let author = resolve_plugin_author(&plugin.manifest);
    let (execution_target, execution_label) = resolve_execution_target(state, &plugin.manifest);
    let installed = match (&state.oci_runner, execution_target) {
        (Some(runner), "local") => runner.has_local_image(&plugin.manifest.image).await,
        _ => true,
    };
    let requires_catalog_level = plugin.manifest.requires_catalog_level;
    let removable = crate::package_install::removal::removable(state, &plugin);
    let removable_versions = match &state.local_runtime {
        Some(runtime) if state.installed_workspace.is_some() => runtime
            .plugin_releases(&plugin.manifest.id)
            .await
            .into_iter()
            .filter(|p| crate::package_install::removal::removable(state, p))
            .map(|p| p.manifest.version)
            .collect(),
        _ => Vec::new(),
    };
    let views = plugin.manifest.views.clone();
    let effective_offers = plugin.manifest.effective_offers();
    PluginSummaryDto {
        manifest: plugin.manifest,
        skill: plugin.skill,
        views,
        effective_offers,
        has_custom_ui: has_ui_bundle,
        ui_entry_url,
        author,
        icon_url,
        execution_target: execution_target.into(),
        execution_label: execution_label.into(),
        requires_catalog_level,
        removable,
        removable_versions,
        detached_versions: Vec::new(),
        installed,
    }
}

async fn offered_run_module_handler(
    State(state): State<AppState>,
    AxumPath(offer_id): AxumPath<String>,
) -> ApiResult<Response> {
    let hub = state.hub_client.as_ref().ok_or_else(|| {
        CompassError::AccessDenied("module offers require a hosted workspace".into())
    })?;
    let (plugin, published_ui) = hub.offered_run_module(&offer_id).await?;
    let mut response = Json(to_plugin_dto(&state, plugin, published_ui).await).into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    Ok(response)
}

async fn list_plugins_handler(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let plugins = if let Some(hub) = &state.hub_client {
        hub.list_plugins_with_ui().await?
    } else {
        state.registry.list_plugins().await?.into_iter().map(|p| (p, None)).collect()
    };
    let dtos =
        futures::future::join_all(plugins.into_iter().map(|(p, ui)| to_plugin_dto(&state, p, ui))).await;
    let mut by_id: std::collections::BTreeMap<_, _> = dtos
        .into_iter()
        .map(|dto| (dto.manifest.id.as_str().to_owned(), dto))
        .collect();
    let mut detached = std::collections::BTreeMap::<String, Vec<LoadedPlugin>>::new();
    for plugin in crate::package_install::removal::detached(&state)? {
        detached
            .entry(plugin.manifest.id.as_str().to_owned())
            .or_default()
            .push(plugin);
    }
    for (id, versions) in detached {
        let card = if let Some(card) = by_id.get_mut(&id) {
            card
        } else {
            let mut card = to_plugin_dto(
                &state,
                versions.last().expect("nonempty source releases").clone(),
                None,
            )
            .await;
            card.installed = false;
            card.removable = false;
            card.has_custom_ui = false;
            card.ui_entry_url = None;
            card.icon_url = None;
            by_id.entry(id).or_insert(card)
        };
        card.detached_versions = versions.into_iter().map(|p| p.manifest.version).collect();
    }
    Ok(Json(
        json!({ "plugins": by_id.into_values().collect::<Vec<_>>() }),
    ))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallPluginRequest {
    plugin_version: Option<String>,
    source: Option<InstallSource>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum InstallSource {
    Workspace,
}

async fn remove_plugin_handler(
    State(state): State<AppState>,
    AxumPath((ns, slug, version)): AxumPath<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let id = PluginId::new(format!("{ns}/{slug}")).map_err(CompassError::Validation)?;
    let removed = crate::package_install::removal::remove(&state, &id, &version).await?;
    Ok(Json(
        json!({"plugin_id":id,"plugin_version":version,"removed":removed}),
    ))
}

async fn install_plugin_handler(
    State(state): State<AppState>,
    AxumPath((ns, slug)): AxumPath<(String, String)>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let request: InstallPluginRequest = if body.is_empty() {
        InstallPluginRequest::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| CompassError::Validation(format!("invalid install request: {e}")))?
    };
    let plugin_id = PluginId::new(format!("{ns}/{slug}")).map_err(CompassError::Validation)?;
    let plugin = if matches!(request.source, Some(InstallSource::Workspace)) {
        let version = request.plugin_version.as_deref().ok_or_else(|| {
            CompassError::Validation(
                "restoring a workspace source requires an exact version".into(),
            )
        })?;
        biocompass_core_traits::validate_requested_plugin_version(Some(version))?;
        crate::package_install::removal::restore_source(&state, &plugin_id, version).await?
    } else if let Some(version) = request.plugin_version {
        biocompass_core_traits::validate_requested_plugin_version(Some(&version))?;
        crate::package_install::install(&state, &plugin_id, &version).await?
    } else {
        match state.registry.get_plugin(&plugin_id).await {
            Err(CompassError::NotFound(_)) if state.installed_workspace.is_some() => {
                crate::package_install::install_latest(&state, &plugin_id).await?
            }
            Err(e) => return Err(e.into()),
            Ok(plugin)
                if plugin
                    .root_dir
                    .as_ref()
                    .is_some_and(|p| p.starts_with(state.data_dir.join("module-releases"))) =>
            {
                crate::package_install::install(&state, &plugin_id, &plugin.manifest.version)
                    .await?
            }
            Ok(plugin) => {
                // Local author and bundled packages retain their existing image-acquisition path.
                let (target, _) = resolve_execution_target(&state, &plugin.manifest);
                let runner = state
                    .oci_runner
                    .as_ref()
                    .filter(|_| target == "local")
                    .ok_or_else(|| {
                        CompassError::AccessDenied("this module cannot be installed locally".into())
                    })?;
                if !runner.has_local_image(&plugin.manifest.image).await {
                    runner.pull_image(&plugin.manifest.image).await?;
                }
                plugin
            }
        }
    };
    let mut dto = to_plugin_dto(&state, plugin, None).await;
    dto.detached_versions = crate::package_install::removal::detached(&state)?
        .into_iter()
        .filter(|p| p.manifest.id == plugin_id)
        .map(|p| p.manifest.version)
        .collect();
    Ok(Json(json!({
        "plugin_id": plugin_id,
        "plugin_version": dto.manifest.version,
        "installed": dto.installed,
        "plugin": dto,
    })))
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
            state.registry.register_plugin(loaded).await.is_ok()
        } else {
            false
        }
    } else {
        false
    };

    let file_entries: Vec<Value> = files
        .into_iter()
        .map(|(relative_path, content)| {
            json!({
                "relative_path": relative_path,
                "content": content,
            })
        })
        .collect();

    Ok(Json(json!({
        "plugin_id": req.plugin_id,
        "template": kind.as_str(),
        "directory": target_dir,
        "registered": registered,
        "files": file_entries,
    })))
}

#[derive(Debug, Clone, Serialize)]
struct InstalledIdeInfo {
    id: String,
    label: String,
    kind: String,
    path: String,
}

#[derive(Debug, Deserialize)]
struct OpenPluginIdeRequest {
    directory: String,
    #[serde(default)]
    ide: Option<String>,
    #[serde(default)]
    custom_command: Option<String>,
}

async fn list_installed_ides_handler(
    State(state): State<AppState>,
) -> ApiResult<Json<Value>> {
    if state.mode != DeploymentMode::Local {
        return Err(CompassError::AccessDenied(
            "IDE discovery is only available in local desktop mode".into(),
        )
        .into());
    }
    let (ides, default_ide_id) = discover_installed_ides(&state.user_plugins_dir);
    Ok(Json(json!({
        "ides": ides,
        "default_ide_id": default_ide_id,
    })))
}

async fn open_plugin_ide_handler(
    State(state): State<AppState>,
    Json(req): Json<OpenPluginIdeRequest>,
) -> ApiResult<Json<Value>> {
    if state.mode != DeploymentMode::Local {
        return Err(CompassError::AccessDenied(
            "opening a local IDE is only available in local desktop mode".into(),
        )
        .into());
    }
    let plugins_root = std::fs::canonicalize(&state.user_plugins_dir).map_err(|e| {
        CompassError::NotFound(format!(
            "plugins directory '{}' unavailable: {e}",
            state.user_plugins_dir.display()
        ))
    })?;
    let requested = std::fs::canonicalize(Path::new(&req.directory)).map_err(|_| {
        CompassError::NotFound(format!("module directory '{}' not found", req.directory))
    })?;
    if !requested.is_dir() || !requested.starts_with(&plugins_root) || requested == plugins_root {
        return Err(CompassError::AccessDenied(
            "requested path must be a scaffolded module subdirectory inside plugins dir".into(),
        )
        .into());
    }
    let launched = launch_dir_in_ide(
        &requested,
        &state.user_plugins_dir,
        req.ide.as_deref(),
        req.custom_command.as_deref(),
    )?;
    Ok(Json(json!({
        "launched": launched,
        "directory": requested,
    })))
}

async fn build_llm_config_response(state: &AppState) -> LlmConfigResponse {
    let active_cfg = state.agent.llm_config().await;
    let local_management_enabled = state.mode == DeploymentMode::Local;
    let (discovered_backends, controls_snap) = if local_management_enabled {
        let active_controls_fut = async {
            match active_cfg.as_ref() {
                Some(cfg) => {
                    resolve_provider_controls(
                        &state.llm_controls_cache,
                        cfg,
                        &state.user_plugins_dir,
                        false,
                    )
                    .await
                }
                None => Default::default(),
            }
        };
        tokio::join!(discover_local_llm_backends(), active_controls_fut)
    } else {
        (Vec::new(), Default::default())
    };
    LlmConfigResponse {
        configured: active_cfg.is_some(),
        display_name: active_cfg.as_ref().map(|c| c.display_name()),
        active: active_cfg.as_ref().map(redact_llm_config),
        active_controls: controls_snap.controls,
        per_model_controls: controls_snap.per_model_controls,
        local_management_enabled,
        discovered_backends,
    }
}

async fn get_llm_config_handler(State(state): State<AppState>) -> Json<LlmConfigResponse> {
    Json(build_llm_config_response(&state).await)
}

async fn update_llm_config_handler(
    State(state): State<AppState>,
    Json(req): Json<UpdateLlmConfigRequest>,
) -> ApiResult<Json<LlmConfigResponse>> {
    if state.mode != DeploymentMode::Local {
        return Err(CompassError::AccessDenied(
            "runtime LLM configuration changes are only permitted in local mode".into(),
        )
        .into());
    }
    let previous = state.agent.llm_config().await;
    let next = normalize_llm_config_request(req, previous.as_ref())?;
    save_saved_llm_config(&state.data_dir, next.as_ref())?;
    state.agent.set_llm_config(next).await;
    Ok(Json(build_llm_config_response(&state).await))
}

async fn test_llm_connection_handler(
    State(state): State<AppState>,
    body: Option<Json<UpdateLlmConfigRequest>>,
) -> ApiResult<Json<Value>> {
    if state.mode != DeploymentMode::Local {
        return Err(CompassError::AccessDenied(
            "local LLM connectivity probes are only permitted in local mode".into(),
        )
        .into());
    }
    let previous = state.agent.llm_config().await;
    let target_cfg = match body {
        Some(Json(req)) if !req.disconnect && req.protocol.is_some() => {
            normalize_llm_config_request(req, previous.as_ref())?
                .ok_or_else(|| CompassError::Validation("no LLM backend provided to test".into()))?
        }
        _ => previous.ok_or_else(|| {
            CompassError::Validation(
                "no LLM backend selected; choose a backend or enter credentials to test".into(),
            )
        })?,
    };
    let result = test_llm_connection(
        &target_cfg,
        &state.user_plugins_dir,
        Some(&state.llm_controls_cache),
    )
    .await;
    Ok(Json(
        serde_json::to_value(result)
            .map_err(|e| CompassError::Internal(format!("encode test result: {e}")))?,
    ))
}

fn spawn_detached(program: &str, args: &[&std::ffi::OsStr]) -> bool {
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

fn open_macos_app(app: &std::ffi::OsStr, dir_os: &std::ffi::OsStr) -> Result<(), CompassError> {
    let output = std::process::Command::new("open")
        .args([std::ffi::OsStr::new("-a"), app, dir_os])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| CompassError::Internal(format!("failed to invoke macOS open: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(CompassError::Validation(if stderr.is_empty() {
            format!("unable to launch application '{}'", app.to_string_lossy())
        } else {
            stderr
        }));
    }
    Ok(())
}

fn is_known_ide_app_name(stem_lower: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "antigravity",
        "android studio",
        "atom",
        "bbedit",
        "clion",
        "coteditor",
        "cursor",
        "datagrip",
        "eclipse",
        "emacs",
        "fleet",
        "goland",
        "helix",
        "intellij idea",
        "lapce",
        "macvim",
        "neovide",
        "netbeans",
        "nova",
        "phpstorm",
        "positron",
        "pycharm",
        "rider",
        "rstudio",
        "rubymine",
        "rustrover",
        "spyder",
        "sublime text",
        "visual studio code",
        "vscode",
        "vscodium",
        "webstorm",
        "windsurf",
        "xcode",
        "zed",
    ];
    PREFIXES.iter().any(|p| stem_lower.starts_with(p))
}

fn collect_macos_ide_bundles() -> Vec<InstalledIdeInfo> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let mut search_roots = vec![std::path::PathBuf::from("/Applications")];
    if let Ok(home) = std::env::var("HOME") {
        let home_apps = std::path::PathBuf::from(home).join("Applications");
        search_roots.push(home_apps.join("JetBrains Toolbox"));
        search_roots.push(home_apps);
    }

    let mut found = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();
    for root in search_roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("app") || !path.is_dir() {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if !is_known_ide_app_name(&stem.to_ascii_lowercase()) {
                continue;
            }
            let path_str = path.display().to_string();
            if seen_paths.insert(path_str.clone()) {
                found.push(InstalledIdeInfo {
                    id: format!("app:{path_str}"),
                    label: stem.to_owned(),
                    kind: "app".to_owned(),
                    path: path_str,
                });
            }
        }
    }
    found
}

fn find_executable_in_path(bin: &str) -> Option<std::path::PathBuf> {
    let candidate = Path::new(bin);
    if candidate.components().count() > 1 {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let full = dir.join(bin);
        if full.is_file() {
            return Some(full);
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat"] {
                let with_ext = dir.join(format!("{bin}.{ext}"));
                if with_ext.is_file() {
                    return Some(with_ext);
                }
            }
        }
    }
    None
}

fn detect_macos_default_app_for_toml(plugins_root: &Path) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    // Look for an existing manifest file so NSWorkspace resolves its registered handler.
    let mut sample_toml = std::fs::read_dir(plugins_root)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("plugin.toml"))
        .find(|p| p.is_file());

    let mut temp_file: Option<std::path::PathBuf> = None;
    if sample_toml.is_none() {
        let probe = std::env::temp_dir().join("biocompass-ide-probe.toml");
        if std::fs::write(&probe, "[plugin]\nid = \"probe/probe\"\n").is_ok() {
            sample_toml = Some(probe.clone());
            temp_file = Some(probe);
        }
    }

    let result = sample_toml.and_then(|toml_path| {
        let path_escaped = toml_path
            .display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let script = format!(
            "ObjC.import(\"AppKit\"); var u = $.NSWorkspace.sharedWorkspace.URLForApplicationToOpenURL($.NSURL.fileURLWithPath(\"{path_escaped}\")); u && u.path ? u.path.js : \"\";"
        );
        let output = std::process::Command::new("osascript")
            .args(["-l", "JavaScript", "-e", &script])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let detected = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if detected.ends_with(".app") && Path::new(&detected).is_dir() {
            Some(detected)
        } else {
            None
        }
    });

    if let Some(probe) = temp_file {
        let _ = std::fs::remove_file(probe);
    }
    result
}

fn discover_installed_ides(plugins_root: &Path) -> (Vec<InstalledIdeInfo>, Option<String>) {
    let mut ides: Vec<InstalledIdeInfo> = Vec::new();
    let mut default_ide_id: Option<String> = None;

    // 1. Respect explicit environment overrides first.
    for env_key in ["BIOCOMPASS_IDE", "VISUAL"] {
        if let Ok(raw) = std::env::var(env_key) {
            let candidate = raw.trim();
            let base = Path::new(candidate)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(candidate);
            if !candidate.is_empty() && !matches!(base, "vi" | "vim" | "nvim" | "nano" | "emacs")
            {
                let id = format!("env:{candidate}");
                if !ides.iter().any(|item| item.id == id) {
                    ides.push(InstalledIdeInfo {
                        id: id.clone(),
                        label: format!("{base} ({env_key})"),
                        kind: "env".to_owned(),
                        path: candidate.to_owned(),
                    });
                }
                if default_ide_id.is_none() {
                    default_ide_id = Some(id);
                }
            }
        }
    }

    // 2. Discover macOS .app bundles neutrally (sorted alphabetically below).
    let mut detected = collect_macos_ide_bundles();

    // Check macOS LaunchServices default handler for .toml code/config files.
    if let Some(default_app_path) = detect_macos_default_app_for_toml(plugins_root) {
        let stem = Path::new(&default_app_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Default App")
            .to_owned();
        // Exclude TextEdit so plain-text fallback doesn't disguise itself as an IDE.
        if !stem.eq_ignore_ascii_case("TextEdit") {
            let app_id = format!("app:{default_app_path}");
            if !detected.iter().any(|item| item.id == app_id) {
                detected.push(InstalledIdeInfo {
                    id: app_id.clone(),
                    label: stem,
                    kind: "app".to_owned(),
                    path: default_app_path,
                });
            }
            if default_ide_id.is_none() {
                default_ide_id = Some(app_id);
            }
        }
    }

    // 3. Discover CLI editors/IDEs available on PATH.
    let cli_candidates = [
        ("Antigravity", "agy"),
        ("Antigravity", "antigravity"),
        ("CLion", "clion"),
        ("Cursor", "cursor"),
        ("Emacs", "emacsclient"),
        ("Fleet", "fleet"),
        ("Geany", "geany"),
        ("GoLand", "goland"),
        ("Helix", "hx"),
        ("IntelliJ IDEA", "idea"),
        ("Kate", "kate"),
        ("Lapce", "lapce"),
        ("MacVim", "mvim"),
        ("MacVim / gVim", "gvim"),
        ("Neovide", "neovide"),
        ("Positron", "positron"),
        ("PyCharm", "pycharm"),
        ("RStudio", "rstudio"),
        ("RustRover", "rustrover"),
        ("Sublime Text", "subl"),
        ("VS Code", "code"),
        ("VS Code Insiders", "code-insiders"),
        ("VSCodium", "codium"),
        ("WebStorm", "webstorm"),
        ("Windsurf", "windsurf"),
        ("Zed", "zed"),
    ];
    for (label, cli) in cli_candidates {
        if let Some(resolved) = find_executable_in_path(cli)
            && !detected
                .iter()
                .any(|item| item.label.eq_ignore_ascii_case(label))
        {
            detected.push(InstalledIdeInfo {
                id: format!("cli:{cli}"),
                label: label.to_owned(),
                kind: "cli".to_owned(),
                path: resolved.display().to_string(),
            });
        }
    }

    detected.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    ides.extend(detected);

    // 4. Include the OS file manager as an always-available option.
    if cfg!(target_os = "macos") {
        ides.push(InstalledIdeInfo {
            id: "os:file-manager".to_owned(),
            label: "Finder".to_owned(),
            kind: "file-manager".to_owned(),
            path: "open".to_owned(),
        });
    } else if cfg!(target_os = "linux") {
        ides.push(InstalledIdeInfo {
            id: "os:file-manager".to_owned(),
            label: "File Manager".to_owned(),
            kind: "file-manager".to_owned(),
            path: "xdg-open".to_owned(),
        });
    } else if cfg!(target_os = "windows") {
        ides.push(InstalledIdeInfo {
            id: "os:file-manager".to_owned(),
            label: "Explorer".to_owned(),
            kind: "file-manager".to_owned(),
            path: "explorer".to_owned(),
        });
    }

    (ides, default_ide_id)
}

fn launch_custom_ide(dir: &Path, raw: &str) -> Result<String, CompassError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(CompassError::Validation(
            "custom IDE command or application name cannot be empty".into(),
        ));
    }
    if trimmed.starts_with('-')
        || trimmed
            .chars()
            .any(|c| c.is_control() || matches!(c, ';' | '&' | '|' | '`' | '$' | '<' | '>'))
    {
        return Err(CompassError::Validation(
            "custom IDE command contains disallowed characters".into(),
        ));
    }

    let dir_os = dir.as_os_str();
    if !trimmed.contains(' ')
        && find_executable_in_path(trimmed).is_some()
        && spawn_detached(trimmed, &[dir_os])
    {
        let label = Path::new(trimmed)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(trimmed);
        return Ok(label.to_owned());
    }

    if cfg!(target_os = "macos") {
        open_macos_app(std::ffi::OsStr::new(trimmed), dir_os)?;
        let label = Path::new(trimmed)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(trimmed);
        return Ok(label.to_owned());
    }

    Err(CompassError::NotFound(format!(
        "IDE executable or application '{trimmed}' was not found on PATH"
    )))
}

fn launch_dir_in_ide(
    dir: &Path,
    plugins_root: &Path,
    ide_id: Option<&str>,
    custom_command: Option<&str>,
) -> Result<String, CompassError> {
    if let Some(custom) = custom_command.map(str::trim).filter(|s| !s.is_empty()) {
        return launch_custom_ide(dir, custom);
    }

    let (ides, default_ide_id) = discover_installed_ides(plugins_root);
    let selected = if let Some(requested_id) = ide_id.map(str::trim).filter(|s| !s.is_empty()) {
        ides.iter()
            .find(|item| item.id == requested_id)
            .ok_or_else(|| {
                CompassError::NotFound(format!("requested IDE '{requested_id}' is not installed"))
            })?
    } else if let Some(ref def_id) = default_ide_id {
        ides.iter()
            .find(|item| &item.id == def_id)
            .or_else(|| ides.first())
            .ok_or_else(|| CompassError::Internal("no IDE or file manager available".into()))?
    } else {
        ides.first()
            .ok_or_else(|| CompassError::Internal("no IDE or file manager available".into()))?
    };

    let dir_os = dir.as_os_str();
    match selected.kind.as_str() {
        "app" => {
            open_macos_app(std::ffi::OsStr::new(&selected.path), dir_os)?;
            Ok(selected.label.clone())
        }
        "env" | "cli" | "file-manager" => {
            if spawn_detached(&selected.path, &[dir_os]) {
                Ok(selected.label.clone())
            } else {
                Err(CompassError::Internal(format!(
                    "failed to launch '{}'",
                    selected.label
                )))
            }
        }
        _ => Err(CompassError::Internal(format!(
            "unsupported IDE kind '{}'",
            selected.kind
        ))),
    }
}

#[derive(Debug, Deserialize, serde::Serialize)]
struct ApiPrepareRunRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    request_key: Option<uuid::Uuid>,
    release_version: Option<u64>,
    marketplace_offer_id: Option<String>,
    workspace_id: Option<uuid::Uuid>,
    plugin_id: String,
    plugin_version: Option<String>,
    #[serde(default)]
    offer_id: Option<String>,
    #[serde(default = "empty_json_obj")]
    params: Value,
    grant_id: Option<String>,
    session_id: Option<String>,
}

fn empty_json_obj() -> Value {
    json!({})
}

/// Reject excess preparations before reading JSON or contacting the hub. Status,
/// confirmations and existing event subscriptions have independent routes.
async fn limit_hosted_prepare(
    State(hub): State<Arc<CompassHubClient>>,
    request: Request,
    next: Next,
) -> Response {
    let _permit = match hub.try_prepare_slot() {
        Ok(permit) => permit,
        Err(error) => {
            let mut response = ApiError(error).into_response();
            response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            return response;
        }
    };
    next.run(request).await
}

async fn prepare_run_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ApiPrepareRunRequest>,
) -> ApiResult<Response> {
    // The upstream hub resolves marketplace entitlements and immutable release pins.
    let explicit_hosted = req.marketplace_offer_id.is_some() || req.release_version.is_some();
    if explicit_hosted {
        let hub = state.hub_client.as_ref().ok_or_else(|| {
            CompassError::Validation("marketplace offers and hosted release selections require a hosted workspace".into())
        })?;
        let body = serde_json::to_vec(&req)
            .map_err(|error| CompassError::Validation(format!("encode hosted run request: {error}")))?;
        return Ok(hub.proxy_raw(reqwest::Method::POST, "/api/v1/runs/prepare", &headers, body).await?);
    }
    let plugin_id = PluginId::new(req.plugin_id).map_err(CompassError::Validation)?;
    biocompass_core_traits::validate_requested_plugin_version(req.plugin_version.as_deref())?;
    let session_id = req
        .session_id
        .map(SessionId::new)
        .transpose()
        .map_err(CompassError::Validation)?;
    let grant = state.resolve_grant(req.grant_id.as_deref()).await?;

    let request = PrepareRunRequest {
        org_id: state.default_org_id.clone(),
        session_id,
        plugin_id,
        plugin_version: req.plugin_version,
        offer_id: req.offer_id,
        params: req.params,
        dataset_grant: grant,
    };
    if let Some(hub) = &state.hub_client {
        let upstream = hub.request_bound_quote(&request, None, req.workspace_id, req.request_key).await?;
        let retry_after = upstream.headers().get(header::RETRY_AFTER).cloned();
        let response = match CompassHubClient::read_bound_quote(&request, upstream).await {
            Ok(quote) => Json(quote).into_response(),
            Err(error) => ApiError::from(error).into_response(),
        };
        return Ok(with_retry_after(response, retry_after));
    }
    Ok(Json(state.dispatcher.prepare_run(request).await?).into_response())
}

fn with_retry_after(mut response: Response, retry_after: Option<HeaderValue>) -> Response {
    if let Some(value) = retry_after {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

#[derive(Debug, Deserialize, Default)]
struct ApiConfirmRunRequest {
    idempotency_key: Option<String>,
}

async fn confirm_run_handler(
    State(state): State<AppState>,
    AxumPath(run_id_raw): AxumPath<String>,
    uri: Uri,
    body: Option<Json<ApiConfirmRunRequest>>,
) -> ApiResult<Response> {
    let run_id = RunId::new(run_id_raw).map_err(CompassError::Validation)?;
    let idem = body
        .and_then(|b| b.0.idempotency_key)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("idem-{run_id}"));

    let (record, retry_after) = if let Some(hub) = &state.hub_client {
        let upstream = hub.submit_confirmation(uri.path(), &idem).await?;
        let retry_after = upstream.headers().get(header::RETRY_AFTER).cloned();
        let record = match crate::cloud_client::decode_hub_json(upstream, "POST /api/v1/runs/:id/confirm").await {
            Ok(record) => record,
            Err(error) => return Ok(with_retry_after(ApiError::from(error).into_response(), retry_after)),
        };
        (record, retry_after)
    } else {
        (state.dispatcher.confirm_run(&run_id, &idem).await?, None)
    };
    let status = if record.confirmation_pending() {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok(with_retry_after((status, Json(record)).into_response(), retry_after))
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
    if state.local_runtime.is_some() && run.org_id != state.default_org_id {
        return Err(CompassError::AccessDenied("FORBIDDEN: run belongs to another org".into()).into());
    }
    Ok(Json(serde_json::to_value(run).unwrap_or(Value::Null)))
}

async fn stream_run_events_handler(
    State(state): State<AppState>,
    AxumPath(run_id_raw): AxumPath<String>,
) -> ApiResult<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>> {
    let run_id = RunId::new(run_id_raw).map_err(CompassError::Validation)?;
    let logs = state.dispatcher.subscribe_logs();
    let runtime = state.local_runtime.as_ref().ok_or_else(|| CompassError::Internal("local run stream unavailable".into()))?;
    let terminal = runtime.subscribe_terminal();
    let run = state.session_store.get_run(&run_id).await?;
    if run.org_id != state.default_org_id {
        return Err(CompassError::AccessDenied("FORBIDDEN: run belongs to another org".into()).into());
    }
    let events = biocompass_core_traits::run_events::RunEvents::new(run.org_id, run_id, logs, terminal, run.status);
    let stream = futures::stream::unfold(events, |mut events| async move {
        let (kind, body) = events.next_message().await?;
        Some((Ok(Event::default().event(kind).data(body.to_string())), events))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn get_run_chain_handler(
    State(state): State<AppState>,
    AxumPath(run_id_raw): AxumPath<String>,
) -> ApiResult<Response> {
    let run_id = RunId::new(run_id_raw).map_err(CompassError::Validation)?;
    let run = state.session_store.get_run(&run_id).await?;
    let key = format!("runs/{}/froglet_chain.json", run_id.as_str());
    let payload = match state.blob_store.get_bytes(&key).await {
        Ok(bytes) => bytes,
        Err(_) => serde_json::to_vec_pretty(&run.froglet_chain)
            .map_err(|e| CompassError::Internal(format!("encode froglet_chain: {e}")))?,
    };
    let mut resp = (StatusCode::OK, payload).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    resp.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(resp)
}

async fn get_run_artifact_handler(
    State(state): State<AppState>,
    AxumPath((run_id_raw, artifact_name)): AxumPath<(String, String)>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if state.hub_client.is_some() {
        // The hub authorizes both listed artifacts and the separate settled result.json.
        let mut response =
            proxy_cloud_route_handler(State(state), Method::GET, uri, headers, Bytes::new()).await?;
        if response.status().is_success() {
            let headers = response.headers_mut();
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
            headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
        }
        return Ok(response);
    }

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

async fn get_ledger_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if let Some(hub) = &state.hub_client {
        return Ok(hub
            .proxy_raw(reqwest::Method::GET, "/api/v1/ledger", &headers, Vec::new())
            .await?);
    }
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
    }))
    .into_response())
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

async fn get_session_handler(
    State(state): State<AppState>,
    AxumPath(session_id_raw): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let session_id = SessionId::new(session_id_raw).map_err(CompassError::Validation)?;
    let session = state.session_store.get_session(&session_id).await?;
    Ok(Json(serde_json::to_value(session).unwrap_or(Value::Null)))
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    title: String,
    active_grant_id: Option<String>,
    project_id: Option<String>,
}

async fn create_session_handler(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> ApiResult<Json<Value>> {
    let active_grant_id = match req.active_grant_id.as_deref() {
        Some(raw) if raw.trim().is_empty() || raw.trim().eq_ignore_ascii_case("none") => {
            "none".to_owned()
        }
        Some(raw) => state
            .resolve_grant(Some(raw))
            .await?
            .map(|g| g.grant_id)
            .unwrap_or_else(|| "none".to_owned()),
        None => state
            .resolve_grant(Some("full"))
            .await?
            .map(|g| g.grant_id)
            .unwrap_or_else(|| "none".to_owned()),
    };
    let mut session = state
        .session_store
        .create_session(&state.default_org_id, &req.title, &active_grant_id)
        .await?;
    if let Some(pid_raw) = req.project_id {
        let pid = ProjectId::new(pid_raw).map_err(CompassError::Validation)?;
        session = state
            .session_store
            .update_session(&session.session_id, None, None, Some(Some(&pid)))
            .await?;
    }
    Ok(Json(serde_json::to_value(session).unwrap_or(Value::Null)))
}

fn deserialize_some<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
struct UpdateSessionRequest {
    title: Option<String>,
    active_grant_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_some")]
    project_id: Option<Option<String>>,
}

async fn update_session_handler(
    State(state): State<AppState>,
    AxumPath(session_id_raw): AxumPath<String>,
    Json(req): Json<UpdateSessionRequest>,
) -> ApiResult<Json<Value>> {
    let session_id = SessionId::new(session_id_raw).map_err(CompassError::Validation)?;
    if let Some(ref grant_id) = req.active_grant_id {
        let _ = state.resolve_grant(Some(grant_id)).await?;
    }
    let parsed_project_id: Option<Option<ProjectId>> = match req.project_id {
        Some(Some(raw)) if !raw.trim().is_empty() => {
            Some(Some(ProjectId::new(raw).map_err(CompassError::Validation)?))
        }
        Some(_) => Some(None),
        None => None,
    };
    let updated = state
        .session_store
        .update_session(
            &session_id,
            req.title.as_deref(),
            req.active_grant_id.as_deref(),
            parsed_project_id
                .as_ref()
                .map(|opt| opt.as_ref()),
        )
        .await?;
    Ok(Json(serde_json::to_value(updated).unwrap_or(Value::Null)))
}

async fn delete_session_handler(
    State(state): State<AppState>,
    AxumPath(session_id_raw): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let session_id = SessionId::new(session_id_raw).map_err(CompassError::Validation)?;
    state.session_store.delete_session(&session_id).await?;
    Ok(Json(json!({ "deleted": true, "session_id": session_id })))
}

async fn list_projects_handler(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let projects = state
        .session_store
        .list_projects(&state.default_org_id)
        .await?;
    Ok(Json(json!({ "projects": projects })))
}

#[derive(Debug, Deserialize)]
struct ProjectNameRequest {
    name: String,
}

async fn create_project_handler(
    State(state): State<AppState>,
    Json(req): Json<ProjectNameRequest>,
) -> ApiResult<Json<Value>> {
    let project = state
        .session_store
        .create_project(&state.default_org_id, &req.name)
        .await?;
    Ok(Json(serde_json::to_value(project).unwrap_or(Value::Null)))
}

async fn update_project_handler(
    State(state): State<AppState>,
    AxumPath(project_id_raw): AxumPath<String>,
    Json(req): Json<ProjectNameRequest>,
) -> ApiResult<Json<Value>> {
    let project_id = ProjectId::new(project_id_raw).map_err(CompassError::Validation)?;
    let updated = state
        .session_store
        .rename_project(&project_id, &req.name)
        .await?;
    Ok(Json(serde_json::to_value(updated).unwrap_or(Value::Null)))
}

async fn delete_project_handler(
    State(state): State<AppState>,
    AxumPath(project_id_raw): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let project_id = ProjectId::new(project_id_raw).map_err(CompassError::Validation)?;
    state.session_store.delete_project(&project_id).await?;
    Ok(Json(json!({ "deleted": true, "project_id": project_id })))
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
    #[serde(default)]
    execution_budget: ExecutionBudget,
    execution_id: Option<uuid::Uuid>,
    #[serde(default)]
    active_module_context: Option<Value>,
}

async fn stream_session_chat_handler(
    State(state): State<AppState>,
    AxumPath(session_id_raw): AxumPath<String>,
    Json(req): Json<ChatTurnRequest>,
) -> ApiResult<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>> {
    let session_id = SessionId::new(session_id_raw).map_err(CompassError::Validation)?;
    let session = state.session_store.get_session(&session_id).await?;
    let active_grant = state.resolve_grant(Some(&session.active_grant_id)).await?;

    let (tx, rx) = mpsc::channel::<AgentStreamEvent>(128);
    if req.message.trim().is_empty() {
        return Err(CompassError::Validation("user message cannot be empty".into()).into());
    }
    if req.execution_budget.requires_hosted_execution() && req.execution_id.is_none() {
        return Err(CompassError::Validation(
            "Provide a unique execution_id for a paid agent allowance. Reuse it when retrying this submission.".into(),
        ).into());
    }
    let agent = match &state.hub_client {
        Some(hub) => {
            let port = BudgetedAgentPort::start(hub.clone(), &session_id, &req.execution_budget, req.execution_id).await?;
            std::sync::Arc::new(state.agent.for_execution(port))
        }
        None if req.execution_budget.requires_hosted_execution() => {
            return Err(CompassError::Validation("Agent ACT preapproval requires a hosted workspace.".into()).into());
        }
        None => state.agent.clone(),
    };
    let msg = req.message;
    let active_module_context = req.active_module_context;
    let caller_auth = current_caller_auth();
    tokio::spawn(CALLER_AUTH.scope(caller_auth, async move {
        if let Err(err) = agent
            .run_turn_with_context(
                &session_id,
                &msg,
                active_grant.as_ref(),
                active_module_context.as_ref(),
                Some(tx.clone()),
            )
            .await
        {
            let payload = match err.hint() {
                Some(hint) => json!({
                    "message": err.user_message(),
                    "code": err.code(),
                    "hint": hint,
                })
                .to_string(),
                None => err.user_message(),
            };
            let _ = tx.send(AgentStreamEvent::Error(payload)).await;
        }
    }));

    let stream = ReceiverStream::new(rx).map(|ev| {
        let data = serde_json::to_string(&ev).unwrap_or_default();
        Ok(Event::default().event("agent").data(data))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// Build a URL path from independently encoded segments. The leading v also makes
// an opaque release label such as "." an ordinary path segment.
fn pinned_ui_path(id: &PluginId, version: &str, asset: &str) -> String {
    let mut url = reqwest::Url::parse("http://localhost/").expect("valid base for relative path");
    url.path_segments_mut().expect("hierarchical URL")
        .push("plugin-ui-version").extend(id.as_str().split('/'))
        .push(&format!("v{version}")).extend(asset.split('/'));
    url.path().to_owned()
}

#[derive(Deserialize)]
struct UiAssetSelection {
    version: Option<String>,
}

async fn serve_pinned_plugin_ui(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath((ns, slug, version, asset)): AxumPath<(String, String, String, String)>,
) -> ApiResult<Response> {
    let version = version.strip_prefix('v')
        .ok_or_else(|| CompassError::Validation("invalid UI release path".into()))?;
    serve_plugin_asset(&state, &headers, ns, slug, asset, Some(version)).await
}

async fn serve_plugin_ui_asset(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath((ns, slug, asset)): AxumPath<(String, String, String)>,
    Query(selection): Query<UiAssetSelection>,
    uri: Uri,
) -> ApiResult<Response> {
    if let Some(hub) = &state.hub_client {
        return Ok(hub.proxy_raw(reqwest::Method::GET,
            uri.path_and_query().map_or(uri.path(), |pq| pq.as_str()), &headers, Vec::new()).await?);
    }
    serve_plugin_asset(&state, &headers, ns, slug, asset, selection.version.as_deref()).await
}

async fn serve_plugin_asset(
    state: &AppState, headers: &HeaderMap, ns: String, slug: String,
    asset_path: String, version: Option<&str>,
) -> ApiResult<Response> {
    let requested = asset_path.trim_start_matches('/');
    let rel = if requested.is_empty() {
        "index.html"
    } else {
        requested
    };

    let plugin_id = PluginId::new(format!("{ns}/{slug}")).map_err(CompassError::Validation)?;
    biocompass_core_traits::validate_requested_plugin_version(version)?;
    if let (Some(version), Some(hub)) = (version, state.hub_client.as_ref()) {
        // The hub authorizes the exact version and returns a release-scoped redirect.
        return Ok(hub.proxy_raw(reqwest::Method::GET, &pinned_ui_path(&plugin_id, version, rel), headers, Vec::new()).await?);
    }
    let plugin = state.registry.get_plugin_version(&plugin_id, version).await?;
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

async fn serve_plugin_icon(
    State(state): State<AppState>,
    AxumPath((ns, slug)): AxumPath<(String, String)>,
) -> ApiResult<Response> {
    let plugin_id = PluginId::new(format!("{ns}/{slug}")).map_err(CompassError::Validation)?;
    let plugin = state.registry.get_plugin(&plugin_id).await?;
    let target = resolve_safe_plugin_icon(&plugin).ok_or_else(|| {
        CompassError::NotFound(format!("plugin '{plugin_id}' has no custom icon file"))
    })?;
    let bytes = std::fs::read(&target)
        .map_err(|e| CompassError::Storage(format!("read plugin icon: {e}")))?;
    let mime = mime_guess::from_path(&target).first_or_octet_stream();
    let mut resp = (StatusCode::OK, bytes).into_response();
    if let Ok(ct) = HeaderValue::from_str(mime.as_ref()) {
        resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
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

// =============================================================================
// Milestone 3 (§7.1–§7.2): Local Saved Views & Local Quarantine Table Import
// =============================================================================

async fn list_saved_views_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if let Some(hub) = &state.hub_client {
        return Ok(hub
            .proxy_raw(
                reqwest::Method::GET,
                "/api/v1/saved-views",
                &headers,
                Vec::new(),
            )
            .await?);
    }
    let sqlite = state
        .local_sqlite
        .as_ref()
        .ok_or_else(|| CompassError::Internal("local SQLite store unavailable".into()))?;
    let views = sqlite.list_saved_views(&state.default_org_id).await?;
    Ok(Json(json!({ "saved_views": views })).into_response())
}

#[derive(Deserialize)]
struct LocalCreateSavedViewReq {
    name: String,
    catalog_id: CatalogId,
    grant_id: String,
    query: QueryAst,
}

async fn create_saved_view_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    if let Some(hub) = &state.hub_client {
        return Ok(hub
            .proxy_raw(
                reqwest::Method::POST,
                "/api/v1/saved-views",
                &headers,
                body.to_vec(),
            )
            .await?);
    }
    let req: LocalCreateSavedViewReq = serde_json::from_slice(&body)
        .map_err(|e| CompassError::Validation(format!("invalid create saved view JSON: {e}")))?;
    let clean_name = req.name.trim().to_owned();
    if clean_name.is_empty() {
        return Err(CompassError::Validation("saved view name cannot be empty".into()).into());
    }

    let sqlite = state
        .local_sqlite
        .as_ref()
        .ok_or_else(|| CompassError::Internal("local SQLite store unavailable".into()))?;
    let runtime = state
        .local_runtime
        .as_ref()
        .ok_or_else(|| CompassError::Internal("local runtime unavailable".into()))?;

    let grant = state
        .resolve_grant(Some(&req.grant_id))
        .await?
        .ok_or_else(|| CompassError::AccessDenied("saved views require a 'rows' grant".into()))?;
    if grant.catalog_id != req.catalog_id {
        return Err(CompassError::AccessDenied(format!(
            "FORBIDDEN: grant '{}' authorizes catalog '{}', not '{}'",
            grant.grant_id, grant.catalog_id, req.catalog_id
        ))
        .into());
    }
    if grant.level != GrantLevel::Rows {
        return Err(CompassError::AccessDenied(format!(
            "FORBIDDEN: grant '{}' has level '{}'; saved views require 'rows'",
            grant.grant_id,
            grant.level.as_str()
        ))
        .into());
    }

    let primary_eng = runtime.get_catalog(&req.catalog_id).await?;
    let primary_sid = req
        .query
        .snapshot_id
        .unwrap_or(primary_eng.manifest().latest_snapshot_id);
    let mut pinned_query = req.query.clone();
    pinned_query.snapshot_id = Some(primary_sid);

    let mut attached_owned = Vec::with_capacity(pinned_query.additional_sources.len());
    for att_pin in &mut pinned_query.additional_sources {
        let att_grant = state
            .resolve_grant(Some(&att_pin.grant_id))
            .await?
            .ok_or_else(|| {
                CompassError::AccessDenied(format!(
                    "attached grant '{}' not found",
                    att_pin.grant_id
                ))
            })?;
        if att_grant.catalog_id != att_pin.catalog_id || att_grant.level != GrantLevel::Rows {
            return Err(CompassError::AccessDenied(format!(
                "attached grant '{}' must have 'rows' access on '{}'",
                att_pin.grant_id, att_pin.catalog_id
            ))
            .into());
        }
        let att_eng = runtime.get_catalog(&att_pin.catalog_id).await?;
        let att_sid = att_pin
            .snapshot_id
            .unwrap_or(att_eng.manifest().latest_snapshot_id);
        let att_digest = att_eng
            .manifest()
            .snapshots
            .iter()
            .find(|s| s.snapshot_id == att_sid)
            .map(|s| s.dataset_digest.clone())
            .unwrap_or_else(|| att_eng.manifest().dataset_digest.clone());
        att_pin.snapshot_id = Some(att_sid);
        attached_owned.push((att_eng, att_grant, att_sid, att_digest));
    }

    let attached_refs: Vec<_> = attached_owned
        .iter()
        .map(|(eng, g, sid, _)| (eng, g, Some(*sid)))
        .collect();
    let selection =
        primary_eng.compute_selection_digest_multi(&grant, &pinned_query, &attached_refs)?;

    // Pin referenced local snapshots so snapshot expiration retains them.
    let _ = state
        .registry
        .set_catalog_snapshot_pin(&req.catalog_id, selection.snapshot_id, true)
        .await;
    let mut additional_sources = Vec::with_capacity(attached_owned.len());
    for (att_eng, att_grant, att_sid, att_digest) in attached_owned {
        let cid = att_eng.manifest().id.clone();
        let _ = state
            .registry
            .set_catalog_snapshot_pin(&cid, att_sid, true)
            .await;
        additional_sources.push(SavedViewSourcePin {
            catalog_id: cid,
            table_name: selection.table_name.clone(),
            snapshot_id: att_sid,
            dataset_digest: att_digest,
            grant_id: att_grant.grant_id.clone(),
            grant_scope_digest: att_grant.scope_digest(),
        });
    }

    let now = chrono::Utc::now();
    let record = SavedViewRecord {
        view_id: format!("sview-{}", Uuid::new_v4().simple()),
        org_id: state.default_org_id.clone(),
        created_by: "local-user".into(),
        name: clean_name,
        plane: SavedViewPlane::Local,
        primary_source: SavedViewSourcePin {
            catalog_id: req.catalog_id,
            table_name: selection.table_name,
            snapshot_id: selection.snapshot_id,
            dataset_digest: selection.dataset_digest,
            grant_id: grant.grant_id.clone(),
            grant_scope_digest: grant.scope_digest(),
        },
        additional_sources,
        primary_key_columns: selection.primary_key_columns,
        query_ast: pinned_query,
        row_count: selection.row_count,
        selection_digest: selection.selection_digest,
        last_status: Some(SavedViewStatus::Verified),
        last_verified_at: Some(now),
        last_detail: Some("Verified locally on this machine".into()),
        local_provenance: true,
        created_at: now,
    };
    sqlite.save_saved_view(&record).await?;

    Ok((StatusCode::CREATED, Json(record)).into_response())
}

async fn delete_saved_view_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(view_id): AxumPath<String>,
) -> ApiResult<Response> {
    if let Some(hub) = &state.hub_client {
        return Ok(hub
            .proxy_raw(
                reqwest::Method::DELETE,
                &format!("/api/v1/saved-views/{}", view_id.trim()),
                &headers,
                Vec::new(),
            )
            .await?);
    }
    let sqlite = state
        .local_sqlite
        .as_ref()
        .ok_or_else(|| CompassError::Internal("local SQLite store unavailable".into()))?;
    let removed = sqlite
        .delete_saved_view(&state.default_org_id, &view_id)
        .await?;
    let remaining = sqlite.list_saved_views(&state.default_org_id).await?;

    let mut candidates = vec![(
        removed.primary_source.catalog_id,
        removed.primary_source.snapshot_id,
    )];
    for add in removed.additional_sources {
        candidates.push((add.catalog_id, add.snapshot_id));
    }
    for (cid, sid) in candidates {
        let still_pinned = remaining.iter().any(|v| {
            (v.primary_source.catalog_id == cid && v.primary_source.snapshot_id == sid)
                || v.additional_sources
                    .iter()
                    .any(|a| a.catalog_id == cid && a.snapshot_id == sid)
        });
        if !still_pinned {
            let _ = state
                .registry
                .set_catalog_snapshot_pin(&cid, sid, false)
                .await;
        }
    }

    Ok(Json(json!({ "deleted": true, "view_id": view_id.trim() })).into_response())
}

async fn verify_saved_view_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(view_id): AxumPath<String>,
    body: Bytes,
) -> ApiResult<Response> {
    if let Some(hub) = &state.hub_client {
        return Ok(hub
            .proxy_raw(
                reqwest::Method::POST,
                &format!("/api/v1/saved-views/{}/verify", view_id.trim()),
                &headers,
                body.to_vec(),
            )
            .await?);
    }
    let sqlite = state
        .local_sqlite
        .as_ref()
        .ok_or_else(|| CompassError::Internal("local SQLite store unavailable".into()))?;
    let runtime = state
        .local_runtime
        .as_ref()
        .ok_or_else(|| CompassError::Internal("local runtime unavailable".into()))?;

    let mut saved = sqlite
        .get_saved_view(&state.default_org_id, &view_id)
        .await?;
    let provenance_label = "Verified locally on this machine".to_string();

    let primary_grant_opt = state
        .resolve_grant(Some(&saved.primary_source.grant_id))
        .await
        .ok()
        .flatten();
    let Some(primary_grant) = primary_grant_opt else {
        let now = chrono::Utc::now();
        let detail = format!(
            "Local grant '{}' is no longer available.",
            saved.primary_source.grant_id
        );
        saved.last_status = Some(SavedViewStatus::AccessChanged);
        saved.last_verified_at = Some(now);
        saved.last_detail = Some(detail.clone());
        let _ = sqlite.save_saved_view(&saved).await;
        return Ok(Json(SavedViewVerificationReport {
            view_id: saved.view_id,
            status: SavedViewStatus::AccessChanged,
            cached: false,
            local_provenance: true,
            provenance_label,
            saved_row_count: saved.row_count,
            saved_selection_digest: saved.selection_digest,
            current_row_count: None,
            current_selection_digest: None,
            detail,
            verified_at: now,
        })
        .into_response());
    };

    if primary_grant.level != GrantLevel::Rows {
        let now = chrono::Utc::now();
        let detail = format!(
            "Local grant '{}' was downgraded to '{}'.",
            primary_grant.grant_id,
            primary_grant.level.as_str()
        );
        saved.last_status = Some(SavedViewStatus::AccessChanged);
        saved.last_verified_at = Some(now);
        saved.last_detail = Some(detail.clone());
        let _ = sqlite.save_saved_view(&saved).await;
        return Ok(Json(SavedViewVerificationReport {
            view_id: saved.view_id,
            status: SavedViewStatus::AccessChanged,
            cached: false,
            local_provenance: true,
            provenance_label,
            saved_row_count: saved.row_count,
            saved_selection_digest: saved.selection_digest,
            current_row_count: None,
            current_selection_digest: None,
            detail,
            verified_at: now,
        })
        .into_response());
    }

    let primary_eng = runtime.get_catalog(&saved.primary_source.catalog_id).await?;
    let mut attached_owned = Vec::with_capacity(saved.additional_sources.len());
    for src in &saved.additional_sources {
        let Some(att_grant) = state
            .resolve_grant(Some(&src.grant_id))
            .await
            .ok()
            .flatten()
            .filter(|g| g.level == GrantLevel::Rows)
        else {
            let now = chrono::Utc::now();
            let detail = format!("Attached grant '{}' is no longer active.", src.grant_id);
            saved.last_status = Some(SavedViewStatus::AccessChanged);
            saved.last_verified_at = Some(now);
            saved.last_detail = Some(detail.clone());
            let _ = sqlite.save_saved_view(&saved).await;
            return Ok(Json(SavedViewVerificationReport {
                view_id: saved.view_id,
                status: SavedViewStatus::AccessChanged,
                cached: false,
                local_provenance: true,
                provenance_label,
                saved_row_count: saved.row_count,
                saved_selection_digest: saved.selection_digest,
                current_row_count: None,
                current_selection_digest: None,
                detail,
                verified_at: now,
            })
            .into_response());
        };
        let att_eng = runtime.get_catalog(&src.catalog_id).await?;
        attached_owned.push((att_eng, att_grant, src.snapshot_id));
    }

    let mut pinned_query = saved.query_ast.clone();
    pinned_query.snapshot_id = Some(saved.primary_source.snapshot_id);
    let pinned_refs: Vec<_> = attached_owned
        .iter()
        .map(|(eng, g, sid)| (eng, g, Some(*sid)))
        .collect();
    let pinned_sel =
        primary_eng.compute_selection_digest_multi(&primary_grant, &pinned_query, &pinned_refs)?;

    let now = chrono::Utc::now();
    if pinned_sel.selection_digest != saved.selection_digest {
        let detail = format!(
            "Grant scope changed on pinned snapshot {} (rows {} -> {}).",
            saved.primary_source.snapshot_id, saved.row_count, pinned_sel.row_count
        );
        saved.last_status = Some(SavedViewStatus::AccessChanged);
        saved.last_verified_at = Some(now);
        saved.last_detail = Some(detail.clone());
        sqlite.save_saved_view(&saved).await?;
        return Ok(Json(SavedViewVerificationReport {
            view_id: saved.view_id,
            status: SavedViewStatus::AccessChanged,
            cached: false,
            local_provenance: true,
            provenance_label,
            saved_row_count: saved.row_count,
            saved_selection_digest: saved.selection_digest,
            current_row_count: Some(pinned_sel.row_count),
            current_selection_digest: Some(pinned_sel.selection_digest),
            detail,
            verified_at: now,
        })
        .into_response());
    }

    let mut head_query = saved.query_ast.clone();
    head_query.snapshot_id = None;
    for att in &mut head_query.additional_sources {
        att.snapshot_id = None;
    }
    let head_refs: Vec<_> = attached_owned
        .iter()
        .map(|(eng, g, _)| (eng, g, None))
        .collect();
    let head_sel =
        primary_eng.compute_selection_digest_multi(&primary_grant, &head_query, &head_refs)?;

    let (status, detail) = if head_sel.selection_digest != saved.selection_digest {
        (
            SavedViewStatus::DataChanged,
            format!(
                "Verified locally on this machine: catalog head advanced to snapshot {} and changed PK selection (rows {} -> {}).",
                head_sel.snapshot_id, saved.row_count, head_sel.row_count
            ),
        )
    } else {
        (
            SavedViewStatus::Verified,
            format!(
                "Verified locally on this machine: {} rows match across pinned snapshot {} and head snapshot {}.",
                head_sel.row_count, saved.primary_source.snapshot_id, head_sel.snapshot_id
            ),
        )
    };

    saved.last_status = Some(status);
    saved.last_verified_at = Some(now);
    saved.last_detail = Some(detail.clone());
    sqlite.save_saved_view(&saved).await?;

    Ok(Json(SavedViewVerificationReport {
        view_id: saved.view_id,
        status,
        cached: false,
        local_provenance: true,
        provenance_label,
        saved_row_count: saved.row_count,
        saved_selection_digest: saved.selection_digest,
        current_row_count: Some(head_sel.row_count),
        current_selection_digest: Some(head_sel.selection_digest),
        detail,
        verified_at: now,
    })
    .into_response())
}

#[derive(Deserialize)]
struct ImportLocalCatalogQuery {
    filename: String,
    format: String,
    mode: String,
    target_catalog_id: String,
    #[serde(default)]
    target_table: Option<String>,
    #[serde(default)]
    catalog_name: Option<String>,
    #[serde(default)]
    catalog_description: Option<String>,
    #[serde(default)]
    organism: Option<String>,
    /// Comma-separated primary-key column names (`e.g. "peptide_id"` or `"allele,peptide"`).
    #[serde(default)]
    primary_key: Option<String>,
}

async fn import_local_catalog_handler(
    State(state): State<AppState>,
    Query(q): Query<ImportLocalCatalogQuery>,
    body: Bytes,
) -> ApiResult<Response> {
    if state.mode != DeploymentMode::Local {
        return Err(CompassError::AccessDenied(
            "local catalog import is only available in --mode local (use /api/v1/uploads/quarantine/* in cloud mode)"
                .into(),
        )
        .into());
    }
    if body.is_empty() {
        return Err(CompassError::Validation("uploaded file body cannot be empty".into()).into());
    }
    let runtime = state
        .local_runtime
        .as_ref()
        .ok_or_else(|| CompassError::Internal("local runtime unavailable".into()))?;

    let target_catalog_id =
        CatalogId::new(q.target_catalog_id.trim()).map_err(CompassError::Validation)?;
    target_catalog_id.ensure_legacy_writable()?;
    let format = IngestFormat::parse(&q.format)?;
    let mode = IngestMode::parse(&q.mode)?;
    let table_name = q
        .target_table
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("records")
        .to_owned();
    let pk_cols: Vec<String> = q
        .primary_key
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();

    let clean_fname = Path::new(q.filename.trim())
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("upload.dat")
        .to_owned();

    let stage_root = state
        .data_dir
        .join("stages")
        .join(format!("local-import-{}", Uuid::new_v4().simple()));
    let in_dir = stage_root.join("in");
    let q_dir = in_dir.join("quarantine");
    let out_dir = stage_root.join("out");
    std::fs::create_dir_all(&q_dir)
        .map_err(|e| CompassError::Storage(format!("create local import stage: {e}")))?;
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| CompassError::Storage(format!("create local import out: {e}")))?;

    let existing_schema = runtime
        .get_catalog(&target_catalog_id)
        .await
        .ok()
        .and_then(|eng| {
            eng.manifest()
                .tables
                .iter()
                .find(|t| t.name == table_name)
                .cloned()
        });

    let spec = QuarantineIngestSpec {
        filename: clean_fname.clone(),
        format,
        mode,
        table_name: table_name.clone(),
        primary_key: pk_cols,
        expected_schema: existing_schema,
    };
    let spec_bytes = serde_json::to_vec_pretty(&spec)
        .map_err(|e| CompassError::Internal(format!("encode ingest_spec: {e}")))?;
    std::fs::write(in_dir.join("ingest_spec.json"), spec_bytes)
        .map_err(|e| CompassError::Storage(format!("write ingest_spec.json: {e}")))?;
    std::fs::write(q_dir.join(&clean_fname), &body)
        .map_err(|e| CompassError::Storage(format!("write uploaded file: {e}")))?;

    let cat_dir = local_catalog_dir(&state.data_dir, &target_catalog_id);
    let cid_clone = target_catalog_id.clone();
    let cname = q
        .catalog_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(target_catalog_id.as_str())
        .to_owned();
    let cdesc = q
        .catalog_description
        .as_deref()
        .map(str::trim)
        .unwrap_or("Local imported DuckLake catalog")
        .to_owned();
    let corg = q
        .organism
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Unspecified")
        .to_owned();
    let commit_msg = format!(
        "Local import '{}' ({}, {}) into table '{}'",
        clean_fname,
        format.as_str(),
        mode.as_str(),
        table_name
    );

    let import_res = tokio::task::spawn_blocking(move || {
        let res = (|| -> biocompass_core_traits::CompassResult<_> {
            let report = validate_and_normalize_quarantine_dir(&in_dir, &out_dir)?;
            let norm_pq = out_dir.join("normalized.parquet");
            let eng = DuckDbCatalogEngine::create_or_import_local_parquet(
                &cat_dir,
                cid_clone,
                &cname,
                &cdesc,
                &corg,
                &table_name,
                mode,
                &report.primary_key,
                &norm_pq,
                &commit_msg,
            )?;
            Ok((report, eng))
        })();
        let _ = std::fs::remove_dir_all(&stage_root);
        res
    })
    .await
    .map_err(|e| CompassError::Internal(format!("local catalog import join error: {e}")))??;

    let (report, eng) = import_res;
    let summary = eng.manifest().clone();
    runtime.register_catalog(eng).await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "report": report,
            "catalog": summary,
        })),
    )
        .into_response())
}
