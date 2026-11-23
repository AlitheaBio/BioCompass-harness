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
    SkillDoc, TemplateKind, render_template_files, validate_scaffold_plugin_id,
    write_template_to_dir,
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
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};
use uuid::Uuid;

#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
struct WebDistAssets;

/// Strict Content-Security-Policy applied to `/plugin-ui/:ns/:slug/*`, `/plugin-ui-version/*`,
/// and `/plugin-ui-access/*` iframe documents (`SEC-17`).
/// `sandbox allow-scripts` forces an opaque `null` origin even if an asset is opened directly;
/// `connect-src 'none'` blocks direct network egress (`fetch`, `XHR`, `WebSocket`), forcing all
/// communication through `@biocompass/plugin-sdk` `postMessage` JSON-RPC 2.0.
pub const PLUGIN_SANDBOX_CSP: &str = "sandbox allow-scripts; default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'";

/// Strict Content-Security-Policy applied to `/plugin-icon/:ns/:slug` icons (`SEC-17`).
const PLUGIN_ICON_CSP: &str = "default-src 'none'; sandbox";

/// Strictly validate that SVG icon bytes contain no active scripts, event handlers, SMIL
/// animations, foreignObject/iframe/object/embed tags, external links, or dangerous URL schemes (`SEC-17`).
pub fn validate_safe_svg_icon(bytes: &[u8]) -> biocompass_core_traits::CompassResult<()> {
    const MAX_ICON_BYTES: usize = 256 * 1024;
    if bytes.is_empty() || bytes.len() > MAX_ICON_BYTES {
        return Err(CompassError::Validation(format!(
            "SVG icon must be 1..={MAX_ICON_BYTES} bytes"
        )));
    }
    if bytes.contains(&0) {
        return Err(CompassError::Validation("SVG icon must not contain NUL bytes".into()));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| CompassError::Validation("SVG icon must be valid UTF-8".into()))?;
    let lower = text.to_ascii_lowercase();
    if !lower.contains("<svg") {
        return Err(CompassError::Validation("SVG icon must contain an <svg> root element".into()));
    }
    if lower.contains("<!doctype") || lower.contains("<!entity") {
        return Err(CompassError::Validation(
            "SVG icon must not contain DOCTYPE or XML ENTITY declarations".into(),
        ));
    }

    let mut saw_svg = false;
    let mut emitter = html5gum::DefaultEmitter::default();
    emitter.naively_switch_states(true);
    for token in html5gum::Tokenizer::new_with_emitter(bytes, emitter).flatten() {
        let html5gum::Token::StartTag(tag) = token else {
            continue;
        };
        let tag_name = String::from_utf8_lossy(tag.name.as_slice()).to_ascii_lowercase();
        if tag_name == "svg" {
            saw_svg = true;
        }
        if matches!(
            tag_name.as_str(),
            "script"
                | "foreignobject"
                | "iframe"
                | "object"
                | "embed"
                | "animate"
                | "animatemotion"
                | "animatetransform"
                | "set"
                | "handler"
                | "use"
                | "image"
                | "a"
                | "style"
                | "meta"
                | "link"
                | "base"
                | "form"
                | "input"
        ) {
            return Err(CompassError::Validation(format!(
                "SVG icon contains forbidden active element '<{tag_name}>'"
            )));
        }
        for (raw_attr, raw_val) in &tag.attributes {
            let attr = String::from_utf8_lossy(raw_attr.as_slice()).to_ascii_lowercase();
            if attr.starts_with("on")
                || matches!(attr.as_str(), "href" | "xlink:href" | "formaction" | "action")
            {
                return Err(CompassError::Validation(format!(
                    "SVG icon contains forbidden attribute '{attr}'"
                )));
            }
            let val_compact: String = String::from_utf8_lossy(raw_val.as_slice())
                .chars()
                .filter(|c| !c.is_ascii_whitespace() && !c.is_control())
                .collect::<String>()
                .to_ascii_lowercase();
            if val_compact.contains("javascript:")
                || val_compact.contains("vbscript:")
                || val_compact.contains("data:text/html")
                || val_compact.contains("data:image/svg")
            {
                return Err(CompassError::Validation(format!(
                    "SVG icon attribute '{attr}' contains forbidden executable URI scheme"
                )));
            }
        }
    }
    if !saw_svg {
        return Err(CompassError::Validation("SVG icon must contain an <svg> element".into()));
    }
    Ok(())
}

fn is_allowed_local_host_authority(state: &AppState, raw_host: &str) -> bool {
    let trimmed = raw_host.trim();
    if trimmed.is_empty() || trimmed.contains('/') || trimmed.contains('@') {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(&format!("http://{trimmed}/")) else {
        return false;
    };
    let Some(host_str) = url.host_str() else {
        return false;
    };
    let unbracketed = host_str.trim_start_matches('[').trim_end_matches(']');
    let is_loopback = unbracketed.eq_ignore_ascii_case("localhost")
        || unbracketed
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if !is_loopback {
        return false;
    }
    if let Some(bound) = state.listen_addr {
        let port = url.port().unwrap_or(80);
        if port == bound.port() {
            return true;
        }
        let env_dev_cors = std::env::var("BIOCOMPASS_DEV_CORS")
            .ok()
            .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
        if (env_dev_cors || state.allow_local_dev_tags || cfg!(debug_assertions))
            && matches!(port, 5173 | 5174)
        {
            return true;
        }
        return false;
    }
    true
}

fn is_allowed_local_origin(state: &AppState, raw_origin: &str) -> bool {
    let Some(rest) = raw_origin.trim().strip_prefix("http://") else {
        return false;
    };
    is_allowed_local_host_authority(state, rest)
}

fn is_mutating_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// Router-level DNS-rebinding & CSRF protection for `DeploymentMode::Local` (`SEC-16`).
async fn local_loopback_guard_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    if state.mode == DeploymentMode::Local {
        let headers = req.headers();
        if let Some(host_val) = headers.get(header::HOST) {
            let ok = host_val
                .to_str()
                .ok()
                .is_some_and(|h| is_allowed_local_host_authority(&state, h));
            if !ok {
                return ApiError(CompassError::AccessDenied(
                    "FORBIDDEN: non-loopback Host header rejected in local mode".into(),
                ))
                .into_response();
            }
        } else if state.listen_addr.is_some() && is_mutating_method(req.method()) {
            return ApiError(CompassError::AccessDenied(
                "FORBIDDEN: missing loopback Host header on local mutating request".into(),
            ))
            .into_response();
        }

        if is_mutating_method(req.method()) {
            if let Some(origin_val) = headers.get(header::ORIGIN) {
                let ok = origin_val
                    .to_str()
                    .ok()
                    .is_some_and(|o| is_allowed_local_origin(&state, o));
                if !ok {
                    return ApiError(CompassError::AccessDenied(
                        "FORBIDDEN: cross-origin mutating request rejected in local mode".into(),
                    ))
                    .into_response();
                }
            }
            if let Some(sfs) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
                let allowed_sfs = match sfs {
                    "same-origin" | "none" => true,
                    "same-site" => headers
                        .get(header::ORIGIN)
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|o| is_allowed_local_origin(&state, o)),
                    _ => false,
                };
                if !allowed_sfs {
                    return ApiError(CompassError::AccessDenied(
                        "FORBIDDEN: cross-site Sec-Fetch-Site rejected on local mutating request".into(),
                    ))
                    .into_response();
                }
            }
        }
    }
    next.run(req).await
}

fn require_json_content_type(headers: &HeaderMap) -> Result<(), Box<Response>> {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(';').next())
        .map(|s| s.trim().to_ascii_lowercase());
    if ct.as_deref() == Some("application/json") {
        Ok(())
    } else {
        Err(Box::new(
            (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                Json(json!({
                    "error": "Content-Type must be application/json",
                    "code": "UNSUPPORTED_MEDIA_TYPE",
                })),
            )
                .into_response(),
        ))
    }
}

fn require_non_simple_upload_content_type(headers: &HeaderMap) -> Result<(), Box<Response>> {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(';').next())
        .map(|s| s.trim().to_ascii_lowercase());
    match ct.as_deref() {
        Some("application/octet-stream" | "application/json") => Ok(()),
        _ => Err(Box::new(
            (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                Json(json!({
                    "error": "Content-Type must be application/octet-stream (CORS-safelisted simple POST types are rejected)",
                    "code": "UNSUPPORTED_MEDIA_TYPE",
                })),
            )
                .into_response(),
        )),
    }
}

fn reject_top_level_plugin_ui_navigation(headers: &HeaderMap) -> Result<(), ApiError> {
    if let Some(dest) = headers.get("sec-fetch-dest").and_then(|v| v.to_str().ok())
        && dest.eq_ignore_ascii_case("document")
    {
        return Err(CompassError::AccessDenied(
            "FORBIDDEN: plugin UI assets may only be rendered inside a sandboxed workbench iframe (top-level document navigation is blocked)".into(),
        )
        .into());
    }
    Ok(())
}

fn sanitize_attachment_filename(raw: &str) -> String {
    let base = Path::new(raw)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("artifact.bin");
    let clean: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .take(128)
        .collect();
    if clean.is_empty() || clean == "." || clean == ".." {
        "artifact.bin".to_owned()
    } else {
        clean
    }
}

fn apply_sandboxed_attachment_headers(resp: &mut Response, filename: &str) {
    let safe_name = sanitize_attachment_filename(filename);
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    if let Ok(cd) = HeaderValue::from_str(&format!("attachment; filename=\"{safe_name}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, cd);
    }
}

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

fn build_cors_layer(state: &AppState) -> CorsLayer {
    let methods = AllowMethods::list([
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
        Method::OPTIONS,
        Method::HEAD,
    ]);
    let headers = AllowHeaders::list([
        header::CONTENT_TYPE,
        header::AUTHORIZATION,
        header::ACCEPT,
        axum::http::HeaderName::from_static("x-api-key"),
        axum::http::HeaderName::from_static("x-workspace-read"),
        axum::http::HeaderName::from_static("x-biocompass-local-owner"),
    ]);

    match state.mode {
        DeploymentMode::Local => {
            let port = state.listen_addr.map_or(8080, |a| a.port());
            let is_loopback = state.listen_addr.is_none_or(|a| a.ip().is_loopback());
            let env_dev_cors = std::env::var("BIOCOMPASS_DEV_CORS")
                .ok()
                .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
            let mut origins = Vec::new();
            if is_loopback {
                for host in ["127.0.0.1", "localhost", "[::1]"] {
                    if let Ok(hv) = HeaderValue::from_str(&format!("http://{host}:{port}")) {
                        origins.push(hv);
                    }
                }
                if env_dev_cors || state.allow_local_dev_tags || cfg!(debug_assertions) {
                    for dev_port in [5173_u16, 5174_u16] {
                        for host in ["127.0.0.1", "localhost", "[::1]"] {
                            if let Ok(hv) = HeaderValue::from_str(&format!("http://{host}:{dev_port}")) {
                                origins.push(hv);
                            }
                        }
                    }
                }
            }
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(origins))
                .allow_methods(methods)
                .allow_headers(headers)
        }
        DeploymentMode::CloudWeb => {
            let mut origins: Vec<HeaderValue> = state
                .allowed_origins
                .iter()
                .filter_map(|o| HeaderValue::from_str(o.trim()).ok())
                .collect();
            if origins.is_empty()
                && let Ok(env_raw) = std::env::var("COMPASS_ALLOWED_ORIGINS")
            {
                origins.extend(
                    env_raw
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .filter_map(|o| HeaderValue::from_str(o).ok()),
                );
            }
            let has_explicit = !origins.is_empty();
            let layer = CorsLayer::new()
                .allow_origin(AllowOrigin::list(origins))
                .allow_methods(methods)
                .allow_headers(headers);
            if has_explicit {
                layer.allow_credentials(true)
            } else {
                layer
            }
        }
    }
}

pub fn build_router(state: AppState) -> Router {
    let cors = build_cors_layer(&state);
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
        .route("/v1/feed", get(proxy_public_marketplace_handler))
        .route("/v1/artifacts/:hash", get(proxy_public_marketplace_artifact_handler))
        .route("/v1/registrations", post(proxy_public_marketplace_handler))
        .route("/api/v1/health", get(get_health))
        .route("/api/v1/marketplace/status", get(get_marketplace_status_handler))
        .route("/api/v1/marketplace/consent", put(update_marketplace_consent_handler))
        .route("/api/v1/marketplace/sync", post(sync_marketplace_handler))
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
        .route("/plugin-ui-access/:ticket/*asset_path", get(serve_plugin_ui_access))
        .route("/plugin-icon/:ns/:slug", get(serve_plugin_icon))
        .fallback(serve_embedded_spa)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            local_loopback_guard_middleware,
        ))
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
    let marketplace = state.marketplace.status().await;
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
        "marketplace_prompt_required": marketplace.prompt_required,
        "marketplace_status": marketplace,
    }))
}

async fn get_marketplace_status_handler(
    State(state): State<AppState>,
) -> Json<crate::marketplace::MarketplaceStatusSnapshot> {
    Json(state.marketplace.status().await)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateMarketplaceConsentRequest {
    decision: crate::marketplace::MarketplaceConsentDecision,
    #[serde(default)]
    hub_url: Option<String>,
    #[serde(default = "default_true")]
    sync_now: bool,
}

async fn update_marketplace_consent_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdateMarketplaceConsentRequest>,
) -> ApiResult<Response> {
    if let Err(resp) = require_json_content_type(&headers) {
        return Ok(*resp);
    }
    if state.mode == DeploymentMode::Local {
        crate::native_import::operator::authorize(&state, &headers)?;
    }
    let snapshot = state
        .marketplace
        .set_consent(req.decision, req.hub_url, req.sync_now)
        .await?;
    Ok(Json(snapshot).into_response())
}

async fn sync_marketplace_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if let Err(resp) = require_json_content_type(&headers) {
        return Ok(*resp);
    }
    if state.mode == DeploymentMode::Local {
        crate::native_import::operator::authorize(&state, &headers)?;
    }
    let snapshot = state.marketplace.sync_now().await?;
    Ok(Json(snapshot).into_response())
}

async fn proxy_public_marketplace_handler(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let Some(hub) = &state.hub_client else {
        return Err(CompassError::NotFound(
            "public marketplace routes are only available in cloud-web mode".into(),
        )
        .into());
    };
    let req_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|e| CompassError::Validation(format!("invalid HTTP method: {e}")))?;
    let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    Ok(hub
        .proxy_raw(req_method, path_and_query, &headers, body.to_vec())
        .await?)
}

async fn proxy_public_marketplace_artifact_handler(
    State(state): State<AppState>,
    AxumPath(hash): AxumPath<String>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let Some(hub) = &state.hub_client else {
        return Err(CompassError::NotFound(
            "public marketplace routes are only available in cloud-web mode".into(),
        )
        .into());
    };
    let valid_hash = !hash.is_empty()
        && hash.len() <= 256
        && !hash.contains("..")
        && hash
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '.'));
    if !valid_hash {
        return Err(CompassError::Validation("invalid marketplace artifact hash".into()).into());
    }
    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/v1/artifacts/");
    Ok(hub
        .proxy_raw(reqwest::Method::GET, path_and_query, &headers, Vec::new())
        .await?)
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
    let catalog_id =
        CatalogId::new(format!("{ns}/{name}")).map_err(CompassError::Validation)?;
    if let Some(hub) = &state.hub_client {
        let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
        let mut upstream = body.to_vec();
        if uri.path().ends_with("/query") {
            let mut payload: Value = serde_json::from_slice(&body)
                .map_err(|e| CompassError::Validation(format!("invalid catalog query JSON: {e}")))?;
            let requested = payload.get("grant_id").and_then(Value::as_str).unwrap_or("full");
            if ["full", "partner", "compute_only", "compute-only", "none", ""].iter()
                .any(|alias| requested.trim().eq_ignore_ascii_case(alias))
            {
                let grant = state
                    .resolve_grant_for_catalog(Some(requested), Some(&catalog_id))
                    .await?
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
    let grant = state
        .resolve_grant_for_catalog(req.grant_id.as_deref().or(Some("full")), Some(&catalog_id))
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
    headers: HeaderMap,
    AxumPath((ns, name)): AxumPath<(String, String)>,
    Json(req): Json<RollbackCatalogRequest>,
) -> ApiResult<Json<Value>> {
    if state.mode == DeploymentMode::Local {
        crate::native_import::operator::authorize(&state, &headers)?;
    }
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
    if icon_rel.is_empty() || icon_rel.contains('\\') || icon_rel.chars().any(char::is_control) {
        return None;
    }
    let ext = Path::new(icon_rel)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)?;
    if !matches!(ext.as_str(), "svg" | "png" | "webp" | "jpg" | "jpeg") {
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

async fn to_plugin_dto(
    state: &AppState,
    plugin: LoadedPlugin,
    hub_meta: crate::cloud_client::HubPluginMeta,
) -> PluginSummaryDto {
    let has_ui_bundle = if let Some(ready) = hub_meta.has_ui_bundle {
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
    let icon_url = if resolve_safe_plugin_icon(&plugin).is_some() || hub_meta.has_remote_icon {
        Some(format!("/plugin-icon/{}", plugin.manifest.id))
    } else {
        None
    };
    let author = resolve_plugin_author(&plugin.manifest);
    let (execution_target, execution_label) = resolve_execution_target(state, &plugin.manifest);
    let installed = match (&state.oci_runner, execution_target) {
        (Some(runner), "local") => {
            if runner.has_local_image(&plugin.manifest.image).await {
                true
            } else if let Some(rt) = &state.local_runtime {
                rt.has_local_bundled_fallback_image(&plugin).await
            } else {
                false
            }
        }
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
        state
            .registry
            .list_plugins()
            .await?
            .into_iter()
            .map(|p| (p, crate::cloud_client::HubPluginMeta::default()))
            .collect()
    };
    let dtos = futures::future::join_all(
        plugins
            .into_iter()
            .map(|(p, meta)| to_plugin_dto(&state, p, meta)),
    )
    .await;
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
                crate::cloud_client::HubPluginMeta::default(),
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
    headers: HeaderMap,
    AxumPath((ns, slug, version)): AxumPath<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    if state.mode == DeploymentMode::Local {
        crate::native_import::operator::authorize(&state, &headers)?;
    }
    let id = PluginId::new(format!("{ns}/{slug}")).map_err(CompassError::Validation)?;
    let removed = crate::package_install::removal::remove(&state, &id, &version).await?;
    Ok(Json(
        json!({"plugin_id":id,"plugin_version":version,"removed":removed}),
    ))
}

async fn install_plugin_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath((ns, slug)): AxumPath<(String, String)>,
    body: Bytes,
) -> ApiResult<Response> {
    if state.mode == DeploymentMode::Local {
        crate::native_import::operator::authorize(&state, &headers)?;
    }
    if let Err(resp) = require_json_content_type(&headers) {
        return Ok(*resp);
    }
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
                    let already_bundled = match &state.local_runtime {
                        Some(rt) => rt.has_local_bundled_fallback_image(&plugin).await,
                        None => false,
                    };
                    if !already_bundled
                        && let Err(pull_err) = runner.pull_image(&plugin.manifest.image).await
                    {
                        if let Some(rt) = &state.local_runtime {
                            let (_, notice) = rt
                                .ensure_local_bundled_fallback_image(&plugin, pull_err)
                                .await?;
                            if let Some(msg) = notice {
                                tracing::warn!("{msg}");
                            }
                        } else {
                            return Err(pull_err.into());
                        }
                    }
                }
                plugin
            }
        }
    };
    let mut dto = to_plugin_dto(&state, plugin, crate::cloud_client::HubPluginMeta::default()).await;
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
    }))
    .into_response())
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
    headers: HeaderMap,
    Json(req): Json<ScaffoldPluginRequest>,
) -> ApiResult<Json<Value>> {
    if state.mode != DeploymentMode::Local {
        return Err(CompassError::AccessDenied(
            "FORBIDDEN: plugin scaffolding is only available in local workstation mode".into(),
        )
        .into());
    }
    crate::native_import::operator::authorize(&state, &headers)?;
    let kind = TemplateKind::parse(&req.template)?;
    let (_ns, slug) = validate_scaffold_plugin_id(&req.plugin_id)?;
    let files = render_template_files(
        kind,
        &req.plugin_id,
        &req.display_name,
        req.include_vite_ui,
    )?;

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
    headers: HeaderMap,
    Json(req): Json<OpenPluginIdeRequest>,
) -> ApiResult<Json<Value>> {
    if state.mode != DeploymentMode::Local {
        return Err(CompassError::AccessDenied(
            "opening a local IDE is only available in local desktop mode".into(),
        )
        .into());
    }
    crate::native_import::operator::authorize(&state, &headers)?;
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
    headers: HeaderMap,
    Json(req): Json<UpdateLlmConfigRequest>,
) -> ApiResult<Json<LlmConfigResponse>> {
    if state.mode != DeploymentMode::Local {
        return Err(CompassError::AccessDenied(
            "runtime LLM configuration changes are only permitted in local mode".into(),
        )
        .into());
    }
    crate::native_import::operator::authorize(&state, &headers)?;
    let previous = state.agent.llm_config().await;
    let next = normalize_llm_config_request(req, previous.as_ref())?;
    save_saved_llm_config(&state.data_dir, next.as_ref())?;
    state.agent.set_llm_config(next).await;
    Ok(Json(build_llm_config_response(&state).await))
}

async fn test_llm_connection_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<UpdateLlmConfigRequest>>,
) -> ApiResult<Response> {
    if state.mode != DeploymentMode::Local {
        return Err(CompassError::AccessDenied(
            "local LLM connectivity probes are only permitted in local mode".into(),
        )
        .into());
    }
    crate::native_import::operator::authorize(&state, &headers)?;
    if let Err(resp) = require_json_content_type(&headers) {
        return Ok(*resp);
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
    )
    .into_response())
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

const ALLOWED_IDE_CLI_BINARIES: &[&str] = &[
    "code",
    "code-insiders",
    "cursor",
    "codium",
    "windsurf",
    "zed",
    "antigravity",
    "agy",
    "subl",
    "idea",
    "pycharm",
    "goland",
    "rustrover",
    "clion",
    "webstorm",
    "fleet",
    "hx",
    "nvim",
    "vim",
    "emacs",
    "rstudio",
    "positron",
    "open",
    "xdg-open",
];

const ALLOWED_MACOS_IDE_APPS: &[&str] = &[
    "Visual Studio Code",
    "Visual Studio Code - Insiders",
    "Cursor",
    "VSCodium",
    "Antigravity",
    "Windsurf",
    "Zed",
    "IntelliJ IDEA",
    "PyCharm",
    "RustRover",
    "GoLand",
    "CLion",
    "WebStorm",
    "Positron",
    "RStudio",
    "Sublime Text",
    "Finder",
];

fn launch_custom_ide(dir: &Path, raw: &str) -> Result<String, CompassError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(CompassError::Validation(
            "custom IDE command or application name cannot be empty".into(),
        ));
    }
    if trimmed.starts_with('-')
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed
            .chars()
            .any(|c| c.is_control() || matches!(c, ';' | '&' | '|' | '`' | '$' | '<' | '>'))
    {
        return Err(CompassError::Validation(
            "custom IDE command must be an allowlisted IDE name without paths or shell metacharacters".into(),
        ));
    }

    let dir_os = dir.as_os_str();
    let lower = trimmed.to_ascii_lowercase();
    if ALLOWED_IDE_CLI_BINARIES.contains(&lower.as_str()) {
        if find_executable_in_path(&lower).is_some() && spawn_detached(&lower, &[dir_os]) {
            return Ok(lower);
        }
        return Err(CompassError::NotFound(format!(
            "IDE executable '{trimmed}' was not found on PATH"
        )));
    }

    if cfg!(target_os = "macos")
        && let Some(matched_app) = ALLOWED_MACOS_IDE_APPS
            .iter()
            .find(|app| app.eq_ignore_ascii_case(trimmed))
    {
        open_macos_app(std::ffi::OsStr::new(*matched_app), dir_os)?;
        return Ok((*matched_app).to_owned());
    }

    Err(CompassError::AccessDenied(format!(
        "FORBIDDEN: '{trimmed}' is not an allowlisted IDE application or CLI executable"
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
        let response = match CompassHubClient::read_bound_quote_json(&request, upstream).await {
            Ok(quote_json) => Json(quote_json).into_response(),
            Err(error) => ApiError::from(error).into_response(),
        };
        return Ok(with_retry_after(response, retry_after));
    }
    let quote = state.dispatcher.prepare_run(request).await?;
    let mut payload = serde_json::to_value(&quote)
        .map_err(|e| CompassError::Internal(format!("encode RunQuote: {e}")))?;
    if let Some(notice) = quote.local_bundled_fallback_notice() {
        payload["execution_notice"] = json!(notice);
    }
    Ok(Json(payload).into_response())
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
    headers: HeaderMap,
    AxumPath(run_id_raw): AxumPath<String>,
    uri: Uri,
    body: Option<Json<ApiConfirmRunRequest>>,
) -> ApiResult<Response> {
    if let Err(resp) = require_json_content_type(&headers) {
        return Ok(*resp);
    }
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
            apply_sandboxed_attachment_headers(&mut response, &artifact_name);
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
    apply_sandboxed_attachment_headers(&mut resp, &art.name);
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

fn apply_plugin_ui_sandbox_headers(resp: &mut Response) {
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PLUGIN_SANDBOX_CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // Allow `<script type="module" crossorigin>` subresources to load from the opaque `null`
    // sandboxed iframe origin (`SEC-17`).
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
}

async fn serve_plugin_ui_access(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
) -> ApiResult<Response> {
    reject_top_level_plugin_ui_navigation(&headers)?;
    let mut resp =
        proxy_cloud_route_handler(State(state), Method::GET, uri, headers, Bytes::new()).await?;
    if resp.status().is_success() {
        apply_plugin_ui_sandbox_headers(&mut resp);
    }
    Ok(resp)
}

async fn serve_pinned_plugin_ui(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath((ns, slug, version, asset)): AxumPath<(String, String, String, String)>,
) -> ApiResult<Response> {
    reject_top_level_plugin_ui_navigation(&headers)?;
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
    reject_top_level_plugin_ui_navigation(&headers)?;
    if let Some(hub) = &state.hub_client {
        let mut resp = hub.proxy_raw(reqwest::Method::GET,
            uri.path_and_query().map_or(uri.path(), |pq| pq.as_str()), &headers, Vec::new()).await?;
        if resp.status().is_success() {
            apply_plugin_ui_sandbox_headers(&mut resp);
        }
        return Ok(resp);
    }
    serve_plugin_asset(&state, &headers, ns, slug, asset, selection.version.as_deref()).await
}

async fn serve_plugin_asset(
    state: &AppState, headers: &HeaderMap, ns: String, slug: String,
    asset_path: String, version: Option<&str>,
) -> ApiResult<Response> {
    reject_top_level_plugin_ui_navigation(headers)?;
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
        let mut resp = hub.proxy_raw(reqwest::Method::GET, &pinned_ui_path(&plugin_id, version, rel), headers, Vec::new()).await?;
        if resp.status().is_success() {
            apply_plugin_ui_sandbox_headers(&mut resp);
        }
        return Ok(resp);
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
    apply_plugin_ui_sandbox_headers(&mut resp);
    Ok(resp)
}

fn apply_plugin_icon_headers(resp: &mut Response) {
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PLUGIN_ICON_CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        axum::http::HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

async fn serve_plugin_icon(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath((ns, slug)): AxumPath<(String, String)>,
) -> ApiResult<Response> {
    let plugin_id = PluginId::new(format!("{ns}/{slug}")).map_err(CompassError::Validation)?;
    let plugin_opt = state.registry.get_plugin(&plugin_id).await.ok();
    if let Some(plugin) = plugin_opt.as_ref()
        && let Some(target) = resolve_safe_plugin_icon(plugin)
    {
        let bytes = std::fs::read(&target)
            .map_err(|e| CompassError::Storage(format!("read plugin icon: {e}")))?;
        let is_svg = target
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("svg"));
        if is_svg {
            validate_safe_svg_icon(&bytes)?;
        }
        let mime = if is_svg {
            "image/svg+xml"
        } else {
            match target
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("png") => "image/png",
                Some("webp") => "image/webp",
                Some("jpg" | "jpeg") => "image/jpeg",
                _ => "application/octet-stream",
            }
        };
        let mut resp = (StatusCode::OK, bytes).into_response();
        resp.headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
        apply_plugin_icon_headers(&mut resp);
        return Ok(resp);
    }
    if let Some(hub) = &state.hub_client {
        let mut resp = hub
            .proxy_raw(
                reqwest::Method::GET,
                &format!("/plugin-icon/{ns}/{slug}"),
                &headers,
                Vec::new(),
            )
            .await?;
        if resp.status().is_success() {
            let ct = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            if ct.starts_with("image/svg") {
                let (parts, body) = resp.into_parts();
                let bytes = axum::body::to_bytes(body, 256 * 1024 + 1)
                    .await
                    .map_err(|e| CompassError::Storage(format!("read proxied icon: {e}")))?;
                validate_safe_svg_icon(&bytes)?;
                resp = Response::from_parts(parts, axum::body::Body::from(bytes));
            }
            apply_plugin_icon_headers(&mut resp);
        }
        return Ok(resp);
    }
    if plugin_opt.is_none() {
        return Err(CompassError::NotFound(format!("plugin '{plugin_id}' is not registered")).into());
    }
    Err(CompassError::NotFound(format!("plugin '{plugin_id}' has no custom icon file")).into())
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
    headers: HeaderMap,
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
    crate::native_import::operator::authorize(&state, &headers)?;
    if let Err(resp) = require_non_simple_upload_content_type(&headers) {
        return Ok(*resp);
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

#[cfg(test)]
mod sec_06_tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    async fn get_health_cors(app: &Router, origin: &str) -> Option<String> {
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/health")
            .header(header::ORIGIN, origin)
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    }

    #[tokio::test]
    async fn sec_06_local_cors_allows_active_loopback_port_and_blocks_external_origins() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = AppState::bootstrap(tmp.path(), DeploymentMode::Local, true)
            .await
            .unwrap();
        let _ = state.enable_local_owner("127.0.0.1:18421".parse().unwrap());
        let app = build_router(state);

        assert_eq!(
            get_health_cors(&app, "https://evil.example").await,
            None,
            "external origin must never be allowed by local CORS"
        );
        assert_eq!(
            get_health_cors(&app, "http://127.0.0.1:9999").await,
            None,
            "unrelated local port must not be allowed by local CORS"
        );
        assert_eq!(
            get_health_cors(&app, "http://127.0.0.1:18421").await.as_deref(),
            Some("http://127.0.0.1:18421")
        );
        assert_eq!(
            get_health_cors(&app, "http://localhost:18421").await.as_deref(),
            Some("http://localhost:18421")
        );
        assert_eq!(
            get_health_cors(&app, "http://127.0.0.1:5173").await.as_deref(),
            Some("http://127.0.0.1:5173")
        );
    }

    #[tokio::test]
    async fn sec_06_cloud_web_cors_defaults_same_origin_and_honours_allowed_origins() {
        let mock_hub = Router::new().route(
            "/api/v1/health",
            get(|| async {
                Json(json!({
                    "status": "ok",
                    "node_id": "froglet-node-mockhub00001",
                    "provider_id": "0".repeat(64),
                    "requester_id": "1".repeat(64),
                    "container_engine": "docker 29.10.0"
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hub_addr = listener.local_addr().unwrap();
        let _hub_task = tokio::spawn(async move {
            let _ = axum::serve(listener, mock_hub).await;
        });

        let tmp = tempfile::tempdir().unwrap();
        let hub_cfg = crate::cloud_client::CompassHubConfig {
            base_url: format!("http://{hub_addr}"),
            bundled_plugins_dir: tmp.path().join("bundled-plugins"),
        };
        let (hub_client, default_org_id, engine_label) =
            CompassHubClient::connect(hub_cfg, std::collections::HashMap::new())
                .await
                .unwrap();
        let hub = Arc::new(hub_client);
        let base_local = AppState::bootstrap(tmp.path(), DeploymentMode::Local, true)
            .await
            .unwrap();
        let mut cloud_state = base_local;
        cloud_state.mode = DeploymentMode::CloudWeb;
        cloud_state.default_org_id = default_org_id;
        cloud_state.container_engine_label = engine_label;
        cloud_state.hub_client = Some(hub);

        // 1. Default (unset/empty) -> strict same-origin (no Access-Control-Allow-Origin emitted).
        let default_app = build_router(cloud_state.clone());
        assert_eq!(
            get_health_cors(&default_app, "https://app.biocompass.example").await,
            None
        );

        // 2. Explicit COMPASS_ALLOWED_ORIGINS allowlists only configured origins.
        cloud_state.allowed_origins = vec!["https://app.biocompass.example".into()];
        let allowlisted_app = build_router(cloud_state);
        assert_eq!(
            get_health_cors(&allowlisted_app, "https://app.biocompass.example")
                .await
                .as_deref(),
            Some("https://app.biocompass.example")
        );
        assert_eq!(
            get_health_cors(&allowlisted_app, "https://evil.example").await,
            None
        );
    }

    #[tokio::test]
    async fn dev_05_serves_downloaded_and_cloud_module_icons_via_plugin_icon_route() {
        let svg_bytes =
            b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 12 12\"><path d=\"M0 0h12v12H0z\"/></svg>";
        let mock_hub = Router::new()
            .route(
                "/api/v1/health",
                get(|| async {
                    Json(json!({
                        "status": "ok",
                        "node_id": "froglet-node-mockhub00002",
                        "provider_id": "0".repeat(64),
                        "requester_id": "1".repeat(64),
                        "container_engine": "docker 29.10.0"
                    }))
                }),
            )
            .route(
                "/api/v1/plugins",
                get(|| async {
                    Json(json!({
                        "plugins": [{
                            "manifest": {
                                "id": "cloud/icon-mod",
                                "name": "Cloud Icon Mod",
                                "version": "1.0.0",
                                "icon": "icon.svg",
                                "image": format!("registry:5000/cloud/icon-mod@sha256:{}", "c".repeat(64))
                            },
                            "skill": null,
                            "has_ui_bundle": false,
                            "icon_url": "/plugin-icon/cloud/icon-mod"
                        }]
                    }))
                }),
            )
            .route(
                "/plugin-icon/cloud/icon-mod",
                get(move || async move {
                    (
                        [(header::CONTENT_TYPE, "image/svg+xml")],
                        svg_bytes.as_slice(),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hub_addr = listener.local_addr().unwrap();
        let _hub_task = tokio::spawn(async move {
            let _ = axum::serve(listener, mock_hub).await;
        });

        let tmp = tempfile::tempdir().unwrap();
        let hub_cfg = crate::cloud_client::CompassHubConfig {
            base_url: format!("http://{hub_addr}"),
            bundled_plugins_dir: tmp.path().join("bundled-plugins"),
        };
        let (hub_client, default_org_id, engine_label) =
            CompassHubClient::connect(hub_cfg, std::collections::HashMap::new())
                .await
                .unwrap();
        let mut cloud_state = AppState::bootstrap(tmp.path(), DeploymentMode::Local, true)
            .await
            .unwrap();

        // 1. Local mode: downloaded/installed module with `content/icon.svg` is served at `/plugin-icon/:ns/:slug`.
        let installed_content = tmp.path().join("module-releases-fixture/content");
        std::fs::create_dir_all(&installed_content).unwrap();
        std::fs::write(installed_content.join("icon.svg"), svg_bytes).unwrap();
        let downloaded_manifest = PluginManifest::from_toml_str(
            &format!(
                "id = \"fixture/downloaded-icon\"\nname = \"Downloaded Icon Mod\"\nversion = \"1.0.0\"\nicon = \"icon.svg\"\nimage = \"registry:5000/fixture/downloaded-icon@sha256:{}\"\n",
                "d".repeat(64)
            ),
            false,
        )
        .unwrap();
        cloud_state
            .registry
            .register_plugin(LoadedPlugin {
                manifest: downloaded_manifest,
                skill: None,
                root_dir: Some(installed_content),
            })
            .await
            .unwrap();
        let local_app = build_router(cloud_state.clone());
        let local_icon_resp = local_app
            .oneshot(
                Request::builder()
                    .uri("/plugin-icon/fixture/downloaded-icon")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(local_icon_resp.status(), StatusCode::OK);
        assert!(
            local_icon_resp
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .contains("default-src 'none'")
        );
        let local_body = axum::body::to_bytes(local_icon_resp.into_body(), 65_536)
            .await
            .unwrap();
        assert_eq!(local_body.as_ref(), svg_bytes);

        // 2. Cloud-web mode: remote module icon is advertised and proxied at `/plugin-icon/:ns/:slug`.
        let hub = Arc::new(hub_client);
        cloud_state.mode = DeploymentMode::CloudWeb;
        cloud_state.default_org_id = default_org_id;
        cloud_state.container_engine_label = engine_label;
        cloud_state.registry = hub.clone();
        cloud_state.hub_client = Some(hub);
        let cloud_app = build_router(cloud_state);

        let list_resp = cloud_app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/plugins")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_resp.status(), StatusCode::OK);
        let list_json: Value = serde_json::from_slice(
            &axum::body::to_bytes(list_resp.into_body(), 1_048_576)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            list_json["plugins"][0]["icon_url"],
            json!("/plugin-icon/cloud/icon-mod")
        );

        let icon_resp = cloud_app
            .oneshot(
                Request::builder()
                    .uri("/plugin-icon/cloud/icon-mod")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(icon_resp.status(), StatusCode::OK);
        assert!(
            icon_resp
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .contains("default-src 'none'")
        );
        let body = axum::body::to_bytes(icon_resp.into_body(), 65_536)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), svg_bytes);
    }

    async fn redeem_test_owner_session(app: &Router, link: &str, host: &str) -> String {
        let ticket = link
            .trim()
            .split_once("#local-owner=")
            .expect("owner link ticket")
            .1;
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/local/owner-session")
                    .header(header::HOST, host)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "ticket": ticket }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 16_384).await.unwrap();
        let val: Value = serde_json::from_slice(&bytes).unwrap();
        val["owner_session"].as_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn sec_16_enforces_loopback_host_origin_owner_token_content_type_and_open_ide_allowlist() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = AppState::bootstrap(tmp.path(), DeploymentMode::Local, true)
            .await
            .unwrap();
        let addr: std::net::SocketAddr = "127.0.0.1:18422".parse().unwrap();
        let link = state.enable_local_owner(addr).unwrap().unwrap();
        let host = "127.0.0.1:18422";
        let app = build_router(state.clone());
        let owner_token = redeem_test_owner_session(&app, &link, host).await;

        // 1. DNS rebinding: non-loopback Host header MUST be rejected (403) even on GET!
        let rebind_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/health")
                    .header(header::HOST, "rebind.attacker.tld:18422")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rebind_resp.status(), StatusCode::FORBIDDEN);

        // 2. Cross-origin / cross-site simple POST CSRF MUST be rejected (403).
        let csrf_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/runs/run-1/confirm")
                    .header(header::HOST, host)
                    .header(header::ORIGIN, "https://evil.example")
                    .header("sec-fetch-site", "cross-site")
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from("pwn"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(csrf_resp.status(), StatusCode::FORBIDDEN);

        // 3. Non-JSON Content-Type on Option<Json<_>> / Bytes mutating endpoints MUST return 415.
        let confirm_ct_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/runs/run-1/confirm")
                    .header(header::HOST, host)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(confirm_ct_resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        let install_ct_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/plugins/alithea/hla-motif-enrichment/install")
                    .header(header::HOST, host)
                    .header("x-biocompass-local-owner", &owner_token)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(install_ct_resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        let import_ct_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/catalogs/import-local?filename=a.csv&format=csv&mode=replace&target_catalog_id=local/t")
                    .header(header::HOST, host)
                    .header("x-biocompass-local-owner", &owner_token)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from("a,b\n1,2\n"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(import_ct_resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        // 4. Missing x-biocompass-local-owner MUST fail with 401 on llm/config, llm/test, open-ide,
        //    scaffold, install, import-local, and rollback!
        for (method, uri, ct, body) in [
            (
                Method::PUT,
                "/api/v1/llm/config",
                "application/json",
                r#"{"protocol":"acp_stdio","command":"/bin/sh","args":["-c","id"]}"#,
            ),
            (
                Method::POST,
                "/api/v1/llm/test",
                "application/json",
                r#"{"protocol":"acp_stdio","command":"/bin/sh","args":["-c","id"]}"#,
            ),
            (
                Method::POST,
                "/api/v1/plugins/open-ide",
                "application/json",
                r#"{"directory":"/tmp","custom_command":"/bin/sh"}"#,
            ),
            (
                Method::POST,
                "/api/v1/plugins/scaffold",
                "application/json",
                r#"{"template":"python-uv","plugin_id":"local/demo","display_name":"Demo"}"#,
            ),
            (
                Method::POST,
                "/api/v1/plugins/alithea/hla-motif-enrichment/install",
                "application/json",
                "{}",
            ),
            (
                Method::POST,
                "/api/v1/catalogs/import-local?filename=a.csv&format=csv&mode=replace&target_catalog_id=local/t",
                "application/octet-stream",
                "a,b\n1,2\n",
            ),
            (
                Method::POST,
                "/api/v1/catalogs/alithea/hla-atlas-v2026.1/rollback",
                "application/json",
                r#"{"target_snapshot_id":1}"#,
            ),
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri(uri)
                        .header(header::HOST, host)
                        .header(header::CONTENT_TYPE, ct)
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::UNAUTHORIZED,
                "SEC-16: {method} {uri} must require x-biocompass-local-owner"
            );
        }

        // 5. Even WITH valid owner token, arbitrary shell commands in llm/config & open-ide are blocked!
        let shell_llm_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/api/v1/llm/config")
                    .header(header::HOST, host)
                    .header("x-biocompass-local-owner", &owner_token)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"protocol":"acp_stdio","command":"/bin/sh","args":["-c","id"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(shell_llm_resp.status(), StatusCode::BAD_REQUEST);

        let mod_dir = state.user_plugins_dir.join("ide-probe");
        std::fs::create_dir_all(&mod_dir).unwrap();
        let shell_ide_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/plugins/open-ide")
                    .header(header::HOST, host)
                    .header("x-biocompass-local-owner", &owner_token)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "directory": mod_dir.to_string_lossy(),
                            "custom_command": "/bin/sh",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            matches!(
                shell_ide_resp.status(),
                StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN
            ),
            "SEC-16: open-ide must reject /bin/sh, got {}",
            shell_ide_resp.status()
        );
    }

    #[tokio::test]
    async fn sec_17_enforces_sandbox_csp_blocks_top_level_document_nav_and_rejects_script_svg() {
        let tmp = tempfile::tempdir().unwrap();
        let state = AppState::bootstrap(tmp.path(), DeploymentMode::Local, true)
            .await
            .unwrap();

        // Register a plugin with UI + malicious SVG icon and another with clean SVG icon.
        let mod_dir = tmp.path().join("ui-sec17");
        std::fs::create_dir_all(mod_dir.join("ui/dist")).unwrap();
        std::fs::write(
            mod_dir.join("ui/dist/index.html"),
            "<!doctype html><html><body>hi</body></html>",
        )
        .unwrap();
        std::fs::write(
            mod_dir.join("evil.svg"),
            b"<svg xmlns=\"http://www.w3.org/2000/svg\" onload=\"alert(1)\"><script>alert(2)</script></svg>",
        )
        .unwrap();
        let evil_manifest = PluginManifest::from_toml_unvalidated(&format!(
            "id = \"fixture/sec17-evil\"\nname = \"Evil\"\nversion = \"1.0.0\"\nicon = \"evil.svg\"\nimage = \"127.0.0.1:5500/fixture/sec17-evil@sha256:{}\"\n[ui]\nentry = \"ui/dist/index.html\"\n",
            "a".repeat(64)
        ))
        .unwrap();
        state
            .local_runtime
            .as_ref()
            .unwrap()
            .register_plugin(LoadedPlugin {
                manifest: evil_manifest,
                skill: None,
                root_dir: Some(mod_dir.clone()),
            })
            .await
            .unwrap();

        let good_dir = tmp.path().join("ui-sec17-good");
        std::fs::create_dir_all(&good_dir).unwrap();
        std::fs::write(
            good_dir.join("icon.svg"),
            b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\"><rect width=\"16\" height=\"16\" fill=\"#000\"/></svg>",
        )
        .unwrap();
        let good_manifest = PluginManifest::from_toml_unvalidated(&format!(
            "id = \"fixture/sec17-good\"\nname = \"Good\"\nversion = \"1.0.0\"\nicon = \"icon.svg\"\nimage = \"127.0.0.1:5500/fixture/sec17-good@sha256:{}\"\n",
            "b".repeat(64)
        ))
        .unwrap();
        state
            .local_runtime
            .as_ref()
            .unwrap()
            .register_plugin(LoadedPlugin {
                manifest: good_manifest,
                skill: None,
                root_dir: Some(good_dir),
            })
            .await
            .unwrap();

        let app = build_router(state);

        // 1. Iframe request to /plugin-ui/* gets strict sandbox CSP + ACAO: *.
        let iframe_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/plugin-ui/fixture/sec17-evil/index.html")
                    .header("sec-fetch-dest", "iframe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(iframe_resp.status(), StatusCode::OK);
        let csp = iframe_resp
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(csp.contains("sandbox allow-scripts"));
        assert!(csp.contains("base-uri 'none'"));
        assert!(csp.contains("form-action 'none'"));
        assert_eq!(
            iframe_resp
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some("*")
        );

        // 2. Direct top-level document navigation to /plugin-ui* and /plugin-ui-access/* MUST be 403!
        for uri in [
            "/plugin-ui/fixture/sec17-evil/index.html",
            "/plugin-ui-version/fixture/sec17-evil/v1.0.0/index.html",
            "/plugin-ui-access/fake-ticket/index.html",
        ] {
            let nav_resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header("sec-fetch-dest", "document")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                nav_resp.status(),
                StatusCode::FORBIDDEN,
                "SEC-17: top-level Sec-Fetch-Dest: document must be rejected on {uri}"
            );
        }

        // 3. Script-bearing SVG icon MUST be rejected on serve (400).
        let evil_icon_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/plugin-icon/fixture/sec17-evil")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(evil_icon_resp.status(), StatusCode::BAD_REQUEST);

        // 4. Clean SVG icon served with sandbox CSP + nosniff + CORP: same-origin.
        let good_icon_resp = app
            .oneshot(
                Request::builder()
                    .uri("/plugin-icon/fixture/sec17-good")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(good_icon_resp.status(), StatusCode::OK);
        assert_eq!(
            good_icon_resp
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .and_then(|v| v.to_str().ok()),
            Some("default-src 'none'; sandbox")
        );
        assert_eq!(
            good_icon_resp
                .headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|v| v.to_str().ok()),
            Some("nosniff")
        );
        assert_eq!(
            good_icon_resp
                .headers()
                .get("cross-origin-resource-policy")
                .and_then(|v| v.to_str().ok()),
            Some("same-origin")
        );
    }

    #[tokio::test]
    async fn sec_18_run_artifacts_served_with_sandbox_csp_nosniff_and_attachment_disposition() {
        let tmp = tempfile::tempdir().unwrap();
        let state = AppState::bootstrap(tmp.path(), DeploymentMode::Local, true)
            .await
            .unwrap();
        let run_id = RunId::from("run-sec18-html");
        let blob_key = "runs/run-sec18-html/artifacts/report.html";
        let html = b"<html><body><script>alert('xss')</script></body></html>";
        let sha = state.blob_store.put_bytes(blob_key, html).await.unwrap();
        let now = chrono::Utc::now();
        let run = biocompass_core_traits::RunRecord {
            run_id: run_id.clone(),
            org_id: state.default_org_id.clone(),
            session_id: None,
            plugin_id: PluginId::new("alithea/hla-motif-enrichment").unwrap(),
            plugin_version: "2.0.0".into(),
            status: biocompass_core_traits::RunStatus::Succeeded,
            admission: None,
            params: json!({}),
            grant: None,
            quote: biocompass_core_traits::RunQuote {
                run_id: run_id.clone(),
                plugin_id: PluginId::new("alithea/hla-motif-enrichment").unwrap(),
                plugin_version: "2.0.0".into(),
                image_ref: format!("127.0.0.1:5500/alithea/test@sha256:{}", "a".repeat(64)),
                target_node_id: biocompass_core_traits::NodeId::new("local").unwrap(),
                offer_id: "standard".into(),
                offer_name: "Standard".into(),
                base_fee_act: CreditAmountAct::ZERO,
                estimated_cost_act: CreditAmountAct::ZERO,
                max_input_bytes: 1024,
                max_output_bytes: 1024,
                max_runtime_sec: 60,
                max_memory_mb: 256,
                workload_hash: "0".repeat(64),
                dataset_grant_summary: None,
                froglet_quote: json!({}),
                expires_at: now,
            },
            result_json: Some(json!({})),
            artifacts: vec![biocompass_core_traits::ArtifactRef {
                name: "report.html".into(),
                blob_key: blob_key.into(),
                media_type: "text/html".into(),
                size_bytes: html.len() as u64,
                sha256: sha,
            }],
            froglet_receipt: None,
            froglet_chain: Vec::new(),
            receipt_verified: false,
            logs: Vec::new(),
            error_message: None,
            created_at: now,
            completed_at: Some(now),
        };
        state.session_store.save_run(&run).await.unwrap();

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/runs/run-sec18-html/artifacts/report.html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .and_then(|v| v.to_str().ok()),
            Some("sandbox")
        );
        assert_eq!(
            resp.headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|v| v.to_str().ok()),
            Some("nosniff")
        );
        let cd = resp
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(cd.starts_with("attachment"), "expected attachment Content-Disposition, got {cd}");
    }

    #[tokio::test]
    async fn sec_24_scaffold_plugin_http_returns_403_in_cloud_web_and_enforces_no_overwrite_locally() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = AppState::bootstrap(tmp.path(), DeploymentMode::Local, true)
            .await
            .unwrap();

        // 1. In CloudWeb mode, POST /api/v1/plugins/scaffold MUST return 403 FORBIDDEN!
        let mut cloud_state = state.clone();
        cloud_state.mode = DeploymentMode::CloudWeb;
        let cloud_app = build_router(cloud_state);
        let cloud_resp = cloud_app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/plugins/scaffold")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"template":"python-uv","plugin_id":"alithea/cloud-pwn","display_name":"Pwn"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cloud_resp.status(), StatusCode::FORBIDDEN);
        assert!(!state.user_plugins_dir.join("cloud-pwn").exists());

        // 2. In Local mode with valid owner token, first scaffold succeeds, second overwrite returns 409 Conflict!
        let addr: std::net::SocketAddr = "127.0.0.1:18423".parse().unwrap();
        let link = state.enable_local_owner(addr).unwrap().unwrap();
        let host = "127.0.0.1:18423";
        let local_app = build_router(state);
        let owner_token = redeem_test_owner_session(&local_app, &link, host).await;

        let req_body = json!({
            "template": "python-uv",
            "plugin_id": "alithea/sec24-mod",
            "display_name": "Safe <script>alert(1)</script> Mod",
            "include_vite_ui": true
        })
        .to_string();
        let first = local_app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/plugins/scaffold")
                    .header(header::HOST, host)
                    .header("x-biocompass-local-owner", &owner_token)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(req_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);

        let second = local_app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/plugins/scaffold")
                    .header(header::HOST, host)
                    .header("x-biocompass-local-owner", &owner_token)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::CONFLICT);
    }
}

#[cfg(test)]
mod milestone_b_tests {
    use super::*;
    use crate::cloud_client::CompassHubConfig;
    use crate::marketplace::{
        MarketplaceConfig, MarketplaceConsentDecision, MarketplaceSyncState,
    };
    use axum::body::Body;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    async fn redeem_owner(app: &Router, link: &str, host: &str) -> String {
        let ticket = link
            .split_once("#local-owner=")
            .expect("owner link fragment")
            .1
            .trim();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/local/owner-session")
                    .header(header::HOST, host)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "ticket": ticket }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 64)
            .await
            .unwrap();
        let parsed: Value = serde_json::from_slice(&bytes).unwrap();
        parsed["owner_session"].as_str().unwrap().to_owned()
    }

    async fn spawn_counting_marketplace_hub() -> (String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_feed = Arc::clone(&hits);
        let hits_art = Arc::clone(&hits);
        let hits_reg = Arc::clone(&hits);

        let router = Router::new()
            .route(
                "/api/v1/health",
                get(|| async {
                    Json(json!({
                        "node_id": "hub-test-node",
                        "provider_id": "provider-alithea",
                        "requester_id": "requester-cloud-web",
                        "container_engine": "mock-hub-engine"
                    }))
                }),
            )
            .route(
                "/api/v1/grants",
                get(|| async { Json(json!({ "grants": [] })) }),
            )
            .route(
                "/api/v1/runs/prepare",
                post(|| async {
                    let now = chrono::Utc::now();
                    Json(json!({
                        "run_id": "run-a2-provider-edge",
                        "plugin_id": "partner-bio/motif-pro",
                        "plugin_version": "1.2.0",
                        "image_ref": format!("ghcr.io/partner/motif-pro@sha256:{}", "b".repeat(64)),
                        "target_node_id": "provider-edge-zurich-01",
                        "offer_id": "standard",
                        "offer_name": "Standard Provider Run",
                        "base_fee_act": 1000,
                        "estimated_cost_act": 2500,
                        "max_input_bytes": 1048576,
                        "max_output_bytes": 1048576,
                        "max_runtime_sec": 300,
                        "max_memory_mb": 2048,
                        "workload_hash": "c".repeat(64),
                        "dataset_grant_summary": null,
                        "froglet_quote": { "schema": "froglet/v1", "type": "quote" },
                        "expires_at": now,
                        "serving_org_id": "org-zurich-proteomics",
                        "external_provider_org": "Zurich Proteomics AG",
                        "external_provider_notice": "Your run parameters and uploaded files will execute on an external provider-edge node operated by Zurich Proteomics AG."
                    }))
                }),
            )
            .route(
                "/v1/feed",
                get(move |uri: Uri| {
                    let hits = Arc::clone(&hits_feed);
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::OK,
                            [(header::ETAG, "\"feed-v1\""), (header::CACHE_CONTROL, "public, max-age=60")],
                            Json(json!({
                                "query": uri.query().unwrap_or(""),
                                "artifacts": [
                                    {
                                        "cursor": 1,
                                        "artifact_hash": "hash-desc-1",
                                        "payload_hash": "ph-desc-1",
                                        "artifact_kind": "descriptor",
                                        "actor_id": "provider-alithea",
                                        "created_at": 1760000000,
                                        "document": { "hash": "hash-desc-1", "signer": "aaaa", "payload": {} }
                                    },
                                    {
                                        "cursor": 2,
                                        "artifact_hash": "hash-offer-1",
                                        "payload_hash": "ph-1",
                                        "artifact_kind": "offer",
                                        "actor_id": "provider-alithea",
                                        "created_at": 1760000001,
                                        "document": {
                                            "hash": "hash-offer-1",
                                            "signer": "aaaa",
                                            "payload": {
                                                "provider_id": "provider-alithea",
                                                "offer_id": "mo-1",
                                                "offer_kind": "biocompass.module.run.v1",
                                                "execution_profile": {
                                                    "access_handles": ["alithea/hla-motif-enrichment@2.0.0"]
                                                }
                                            }
                                        }
                                    },
                                    {
                                        "cursor": 3,
                                        "artifact_hash": "hash-offer-2",
                                        "payload_hash": "ph-2",
                                        "artifact_kind": "offer",
                                        "actor_id": "provider-partner",
                                        "created_at": 1760000002,
                                        "document": {
                                            "hash": "hash-offer-2",
                                            "signer": "bbbb",
                                            "payload": {
                                                "provider_id": "provider-partner",
                                                "offer_id": "do-2",
                                                "offer_kind": "dataset.ducklake.v1",
                                                "execution_profile": {
                                                    "access_handles": ["alithea/hla-atlas-v2026.1"]
                                                }
                                            }
                                        }
                                    }
                                ],
                                "active_offer_hashes": ["hash-offer-1", "hash-offer-2"],
                                "has_more": false,
                                "next_cursor": 3
                            })),
                        )
                    }
                }),
            )
            .route(
                "/v1/artifacts/:hash",
                get(move |AxumPath(hash): AxumPath<String>| {
                    let hits = Arc::clone(&hits_art);
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::OK,
                            [
                                (header::CONTENT_TYPE, "application/octet-stream"),
                                (header::HeaderName::from_static("x-content-sha256"), "deadbeef"),
                            ],
                            format!("artifact-bytes:{hash}"),
                        )
                    }
                }),
            )
            .route(
                "/v1/registrations",
                post(move |body: Bytes| {
                    let hits = Arc::clone(&hits_reg);
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                        (
                            StatusCode::CREATED,
                            [(header::RETRY_AFTER, "5")],
                            Json(json!({
                                "status": "pending",
                                "received": parsed
                            })),
                        )
                    }
                }),
            );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        (format!("http://{addr}"), hits)
    }

    #[tokio::test]
    async fn a1_http_fresh_local_zero_outbound_before_consent_and_persists_decline() {
        let (hub_url, hits) = spawn_counting_marketplace_hub().await;
        let tmp = tempfile::tempdir().unwrap();
        let cfg = MarketplaceConfig {
            mode: DeploymentMode::Local,
            default_hub_url: hub_url.clone(),
            explicit_hub_url: None,
            airgapped: false,
        };
        let mut state = AppState::bootstrap_with_marketplace(
            tmp.path(),
            DeploymentMode::Local,
            true,
            cfg.clone(),
        )
        .await
        .unwrap();
        let addr: std::net::SocketAddr = "127.0.0.1:18491".parse().unwrap();
        let link = state.enable_local_owner(addr).unwrap().unwrap();
        let host = "127.0.0.1:18491";
        let app = build_router(state);
        let owner_token = redeem_owner(&app, &link, host).await;

        // 1. Fresh local directory: health + status + plugins make ZERO outbound requests.
        for uri in ["/api/v1/health", "/api/v1/marketplace/status", "/api/v1/plugins"] {
            let resp = app
                .clone()
                .oneshot(Request::builder().uri(uri).header(header::HOST, host).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "GET {uri}");
        }
        assert_eq!(hits.load(Ordering::SeqCst), 0, "fresh boot must make zero outbound marketplace calls");

        // 2. Sync before consent is rejected (403) with zero outbound traffic.
        let pre_sync = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/marketplace/sync")
                    .header(header::HOST, host)
                    .header("x-biocompass-local-owner", &owner_token)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(pre_sync.status(), StatusCode::FORBIDDEN);
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        // 3. Unauthenticated consent update is rejected (401).
        let unauth = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/api/v1/marketplace/consent")
                    .header(header::HOST, host)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"decision":"accepted"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        // 4. Decline persists across restart with ZERO outbound requests.
        let decline = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/api/v1/marketplace/consent")
                    .header(header::HOST, host)
                    .header("x-biocompass-local-owner", &owner_token)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"decision":"declined"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(decline.status(), StatusCode::OK);
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        // Restart AppState on same data_dir.
        let mut restarted_state = AppState::bootstrap_with_marketplace(
            tmp.path(),
            DeploymentMode::Local,
            true,
            cfg,
        )
        .await
        .unwrap();
        let link2 = restarted_state.enable_local_owner(addr).unwrap().unwrap();
        let app2 = build_router(restarted_state);
        let owner_token2 = redeem_owner(&app2, &link2, host).await;

        let status_resp = app2
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/marketplace/status")
                    .header(header::HOST, host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status_bytes = axum::body::to_bytes(status_resp.into_body(), 1024 * 64)
            .await
            .unwrap();
        let status_json: Value = serde_json::from_slice(&status_bytes).unwrap();
        assert_eq!(status_json["prompt_required"], false);
        assert_eq!(status_json["consent"], "declined");
        assert_eq!(status_json["state"], "disconnected");
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        // 5. Explicit accept triggers sync and transitions to connected.
        let accept = app2
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/api/v1/marketplace/consent")
                    .header(header::HOST, host)
                    .header("x-biocompass-local-owner", &owner_token2)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"decision":"accepted","sync_now":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accept.status(), StatusCode::OK);
        let accept_bytes = axum::body::to_bytes(accept.into_body(), 1024 * 64)
            .await
            .unwrap();
        let accept_json: Value = serde_json::from_slice(&accept_bytes).unwrap();
        assert_eq!(accept_json["state"], "connected");
        assert_eq!(accept_json["consent"], "accepted");
        assert_eq!(accept_json["feed_offer_count"], 2);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a1_http_airgapped_suppresses_prompt_and_blocks_all_outbound_calls() {
        let (hub_url, hits) = spawn_counting_marketplace_hub().await;
        let tmp = tempfile::tempdir().unwrap();
        let cfg = MarketplaceConfig {
            mode: DeploymentMode::Local,
            default_hub_url: hub_url.clone(),
            explicit_hub_url: Some(hub_url),
            airgapped: true,
        };
        let mut state = AppState::bootstrap_with_marketplace(
            tmp.path(),
            DeploymentMode::Local,
            true,
            cfg,
        )
        .await
        .unwrap();
        let addr: std::net::SocketAddr = "127.0.0.1:18492".parse().unwrap();
        let link = state.enable_local_owner(addr).unwrap().unwrap();
        let host = "127.0.0.1:18492";
        let app = build_router(state);
        let owner_token = redeem_owner(&app, &link, host).await;

        let health = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .header(header::HOST, host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        let health_json: Value =
            serde_json::from_slice(&axum::body::to_bytes(health.into_body(), 1024 * 64).await.unwrap())
                .unwrap();
        assert_eq!(health_json["marketplace_prompt_required"], false);
        assert_eq!(health_json["marketplace_status"]["state"], "air-gapped");
        assert_eq!(health_json["marketplace_status"]["airgapped"], true);

        let consent = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/api/v1/marketplace/consent")
                    .header(header::HOST, host)
                    .header("x-biocompass-local-owner", &owner_token)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"decision":"accepted"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(consent.status(), StatusCode::FORBIDDEN);

        let sync = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/marketplace/sync")
                    .header(header::HOST, host)
                    .header("x-biocompass-local-owner", &owner_token)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(sync.status(), StatusCode::FORBIDDEN);
        assert_eq!(hits.load(Ordering::SeqCst), 0, "air-gapped mode must never contact marketplace");
    }

    #[tokio::test]
    async fn a2_and_b1_cloud_web_preserves_external_provider_consent_and_proxies_public_marketplace() {
        let (hub_url, hits) = spawn_counting_marketplace_hub().await;
        let tmp = tempfile::tempdir().unwrap();

        let (hub_client, default_org_id, engine_label) = CompassHubClient::connect(
            CompassHubConfig {
                base_url: hub_url.clone(),
                bundled_plugins_dir: tmp.path().join("bundled"),
            },
            std::collections::HashMap::new(),
        )
        .await
        .unwrap();
        let hub = Arc::new(hub_client);

        let mut state = AppState::bootstrap(tmp.path(), DeploymentMode::Local, true)
            .await
            .unwrap();
        // Verify local mode returns 404 for public /v1/feed.
        let local_app = build_router(state.clone());
        let local_feed = local_app
            .oneshot(Request::builder().uri("/v1/feed").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(local_feed.status(), StatusCode::NOT_FOUND);
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        // Switch state into CloudWeb backed by our mock hub.
        state.mode = DeploymentMode::CloudWeb;
        state.default_org_id = default_org_id;
        state.container_engine_label = engine_label;
        state.hub_client = Some(Arc::clone(&hub));
        state.registry = hub.clone();
        state.dispatcher = hub.clone();
        state.session_store = hub.clone();
        state.blob_store = hub.clone();
        state.payment_gateway = hub;
        state.marketplace = Arc::new(
            crate::marketplace::MarketplaceClient::new(
                tmp.path(),
                MarketplaceConfig {
                    mode: DeploymentMode::CloudWeb,
                    default_hub_url: hub_url.clone(),
                    explicit_hub_url: Some(hub_url),
                    airgapped: false,
                },
            )
            .unwrap(),
        );
        let cloud_app = build_router(state);

        // [A2] POST /api/v1/runs/prepare preserves serving_org_id, external_provider_org, external_provider_notice.
        let prep_resp = cloud_app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/runs/prepare")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "plugin_id": "partner-bio/motif-pro",
                            "plugin_version": "1.2.0",
                            "offer_id": "standard",
                            "params": { "alpha": 1 }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(prep_resp.status(), StatusCode::OK);
        let prep_json: Value = serde_json::from_slice(
            &axum::body::to_bytes(prep_resp.into_body(), 1024 * 64)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(prep_json["target_node_id"], "provider-edge-zurich-01");
        assert_eq!(prep_json["serving_org_id"], "org-zurich-proteomics");
        assert_eq!(prep_json["external_provider_org"], "Zurich Proteomics AG");
        assert!(
            prep_json["external_provider_notice"]
                .as_str()
                .is_some_and(|s| s.contains("Zurich Proteomics AG"))
        );

        // [B1] GET /v1/feed proxied with query & headers.
        let feed_resp = cloud_app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/feed?kind=run")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(feed_resp.status(), StatusCode::OK);
        assert_eq!(
            feed_resp.headers().get(header::ETAG).and_then(|v| v.to_str().ok()),
            Some("\"feed-v1\"")
        );
        let feed_json: Value = serde_json::from_slice(
            &axum::body::to_bytes(feed_resp.into_body(), 1024 * 64)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(feed_json["query"], "kind=run");
        assert_eq!(feed_json["artifacts"].as_array().map(Vec::len), Some(3));
        assert_eq!(feed_json["active_offer_hashes"].as_array().map(Vec::len), Some(2));

        // [B1] GET /v1/artifacts/:hash proxied with digest header; traversal rejected.
        let art_resp = cloud_app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/artifacts/sha256:0123456789abcdef")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(art_resp.status(), StatusCode::OK);
        assert_eq!(
            art_resp
                .headers()
                .get("x-content-sha256")
                .and_then(|v| v.to_str().ok()),
            Some("deadbeef")
        );
        let art_body = axum::body::to_bytes(art_resp.into_body(), 1024 * 64)
            .await
            .unwrap();
        assert_eq!(&art_body[..], b"artifact-bytes:sha256:0123456789abcdef");

        let bad_art = cloud_app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/artifacts/bad..hash")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_art.status(), StatusCode::BAD_REQUEST);

        // [B1] POST /v1/registrations proxied with body & status.
        let reg_resp = cloud_app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/registrations")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"peer_url":"https://client.hospital.org"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(reg_resp.status(), StatusCode::CREATED);
        assert_eq!(
            reg_resp
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("5")
        );
        let reg_json: Value = serde_json::from_slice(
            &axum::body::to_bytes(reg_resp.into_body(), 1024 * 64)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(reg_json["status"], "pending");
        assert_eq!(reg_json["received"]["peer_url"], "https://client.hospital.org");
        assert_eq!(hits.load(Ordering::SeqCst), 3);

        // Confirm status enum serialization strings match DEV-07 UI contract.
        assert_eq!(
            serde_json::to_string(&MarketplaceSyncState::AirGapped).unwrap(),
            "\"air-gapped\""
        );
        assert_eq!(
            serde_json::to_string(&MarketplaceConsentDecision::Accepted).unwrap(),
            "\"accepted\""
        );
    }
}
