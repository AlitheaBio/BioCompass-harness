//! Ephemeral per-turn loopback Streamable HTTP MCP server (`127.0.0.1:0/mcp/<token>`).
//!
//! Exposes BioCompass's `allowed_tool_definitions()` to local ACP / subscription CLI agents
//! strictly through `execute_agent_tool(...)`, preserving:
//! - DatasetGrant + DuckDB masking enforcement
//! - BlobStore spill above `spill_threshold_bytes`
//! - `SessionStore` audit logging (`ToolCall` / `ToolResult`)
//! - Live UI SSE forwarding (`ModuleDraftUpdated`, `RunPrepared`, `ToolCall`, `ToolResult`)
//! - Physical omission of `confirm_run` (Two-Step Execution Consent Gate)

use crate::AgentStreamEvent;
use crate::llm::LlmToolCall;
use crate::tools::{
    AgentExecutionPort, allowed_tool_definitions_for, execute_agent_tool_with_options,
    spill_tool_output_if_needed,
};
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use biocompass_core_traits::{
    BlobStore, CompassError, CompassResult, DatasetGrant, OrgId, RunQuote, SessionEventKind,
    SessionId, SessionStore,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Accumulated metrics collected while a local CLI/ACP agent invoked MCP tools during a turn.
#[derive(Debug, Default, Clone)]
pub struct McpTurnMetrics {
    pub tool_calls_executed: usize,
    pub prepared_quotes: Vec<RunQuote>,
}

#[derive(Clone)]
struct McpAppState {
    token: String,
    org_id: OrgId,
    session_id: SessionId,
    active_grant: Option<DatasetGrant>,
    execution_port: Arc<dyn AgentExecutionPort>,
    session_store: Arc<dyn SessionStore>,
    blob_store: Arc<dyn BlobStore>,
    scaffold_root_dir: PathBuf,
    allow_scaffold: bool,
    spill_threshold_bytes: usize,
    stream_tx: Option<mpsc::Sender<AgentStreamEvent>>,
    metrics: Arc<Mutex<McpTurnMetrics>>,
}

/// Running loopback MCP server scoped to a single agent turn.
pub struct ScopedMcpServer {
    url: String,
    metrics: Arc<Mutex<McpTurnMetrics>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl ScopedMcpServer {
    /// Spawn a temporary loopback HTTP MCP server bound to `127.0.0.1:0`.
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        org_id: OrgId,
        session_id: SessionId,
        active_grant: Option<DatasetGrant>,
        execution_port: Arc<dyn AgentExecutionPort>,
        session_store: Arc<dyn SessionStore>,
        blob_store: Arc<dyn BlobStore>,
        scaffold_root_dir: PathBuf,
        spill_threshold_bytes: usize,
        stream_tx: Option<mpsc::Sender<AgentStreamEvent>>,
    ) -> CompassResult<Self> {
        Self::start_with_options(
            org_id,
            session_id,
            active_grant,
            execution_port,
            session_store,
            blob_store,
            scaffold_root_dir,
            spill_threshold_bytes,
            stream_tx,
            true,
        )
        .await
    }

    /// Spawn a temporary loopback HTTP MCP server with explicit `allow_scaffold` policy (`SEC-24`).
    #[allow(clippy::too_many_arguments)]
    pub async fn start_with_options(
        org_id: OrgId,
        session_id: SessionId,
        active_grant: Option<DatasetGrant>,
        execution_port: Arc<dyn AgentExecutionPort>,
        session_store: Arc<dyn SessionStore>,
        blob_store: Arc<dyn BlobStore>,
        scaffold_root_dir: PathBuf,
        spill_threshold_bytes: usize,
        stream_tx: Option<mpsc::Sender<AgentStreamEvent>>,
        allow_scaffold: bool,
    ) -> CompassResult<Self> {
        let token = Uuid::new_v4().simple().to_string();
        let metrics = Arc::new(Mutex::new(McpTurnMetrics::default()));
        let state = McpAppState {
            token: token.clone(),
            org_id,
            session_id,
            active_grant,
            execution_port,
            session_store,
            blob_store,
            scaffold_root_dir,
            allow_scaffold,
            spill_threshold_bytes,
            stream_tx,
            metrics: Arc::clone(&metrics),
        };

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| CompassError::Internal(format!("bind ephemeral MCP port: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| CompassError::Internal(format!("read ephemeral MCP addr: {e}")))?;

        let app = Router::new()
            .route("/mcp/:token", post(handle_mcp_post).get(method_not_allowed))
            .with_state(state);

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });

        Ok(Self {
            url: format!("http://{addr}/mcp/{token}"),
            metrics,
            shutdown_tx: Some(shutdown_tx),
            handle,
        })
    }

    /// Loopback Streamable HTTP URL for this turn (`http://127.0.0.1:<port>/mcp/<token>`).
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Snapshot metrics recorded so far without stopping the server.
    pub async fn metrics_snapshot(&self) -> McpTurnMetrics {
        self.metrics.lock().await.clone()
    }

    /// Gracefully shut down the loopback MCP server and return recorded tool metrics.
    pub async fn finish(mut self) -> McpTurnMetrics {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = (&mut self.handle).await;
        self.metrics.lock().await.clone()
    }
}

impl Drop for ScopedMcpServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }
}

async fn method_not_allowed() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

fn json_rpc_ok(id: Value, result: Value) -> Response {
    let body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        Json(body),
    )
        .into_response()
}

fn json_rpc_err(id: Value, code: i64, message: impl Into<String>) -> Response {
    let body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message.into(),
        }
    });
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        Json(body),
    )
        .into_response()
}

async fn handle_mcp_post(
    State(state): State<McpAppState>,
    AxumPath(token): AxumPath<String>,
    Json(req): Json<JsonRpcRequest>,
) -> Response {
    if token != state.token {
        return StatusCode::NOT_FOUND.into_response();
    }

    // Notifications (no `id`) do not expect a JSON-RPC response body.
    let Some(id) = req.id else {
        return StatusCode::ACCEPTED.into_response();
    };

    match req.method.as_str() {
        "initialize" => {
            let requested = req
                .params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .filter(|v| v.starts_with("2024-") || v.starts_with("2025-"))
                .unwrap_or("2024-11-05");
            json_rpc_ok(
                id,
                json!({
                    "protocolVersion": requested,
                    "capabilities": {
                        "tools": { "listChanged": false }
                    },
                    "serverInfo": {
                        "name": "biocompass-mcp",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }),
            )
        }
        "ping" => json_rpc_ok(id, json!({})),
        "tools/list" => {
            let tools: Vec<Value> = allowed_tool_definitions_for(state.allow_scaffold)
                .into_iter()
                .map(|t| {
                    let read_only = matches!(
                        t.name.as_str(),
                        "list_catalogs"
                            | "query_catalog"
                            | "list_plugins"
                            | "get_plugin_details"
                            | "list_session_artifacts"
                    );
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": t.input_schema,
                        "annotations": {
                            "readOnlyHint": read_only,
                            "destructiveHint": false,
                            "openWorldHint": false,
                        }
                    })
                })
                .collect();
            json_rpc_ok(id, json!({ "tools": tools }))
        }
        "tools/call" => handle_tools_call(&state, id, &req.params).await,
        _ => json_rpc_err(id, -32601, format!("method not found: {}", req.method)),
    }
}

async fn handle_tools_call(state: &McpAppState, id: Value, params: &Value) -> Response {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return json_rpc_err(id, -32602, "tools/call requires params.name");
    };

    if !allowed_tool_definitions_for(state.allow_scaffold)
        .iter()
        .any(|t| t.name == name)
    {
        return json_rpc_ok(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&json!({
                        "error": format!("tool '{name}' is not allowed or requires explicit user consent")
                    })).unwrap_or_default(),
                }],
                "isError": true,
            }),
        );
    }

    let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let call_id = format!("mcp_{}", Uuid::new_v4().simple());
    let tc = LlmToolCall {
        id: call_id.clone(),
        name: name.to_owned(),
        arguments: arguments.clone(),
    };

    if let Err(err) = state
        .session_store
        .append_event(
            &state.session_id,
            SessionEventKind::ToolCall,
            &tc.name,
            json!({
                "id": tc.id,
                "arguments": tc.arguments,
                "via": "mcp",
            }),
        )
        .await
    {
        return json_rpc_err(id, -32603, format!("persist ToolCall: {err}"));
    }

    if let Some(tx) = &state.stream_tx {
        let _ = tx
            .send(AgentStreamEvent::ToolCall {
                id: tc.id.clone(),
                name: tc.name.clone(),
                arguments: tc.arguments.clone(),
            })
            .await;
    }

    let raw_val = execute_agent_tool_with_options(
        &tc,
        &state.org_id,
        &state.session_id,
        state.active_grant.as_ref(),
        state.execution_port.as_ref(),
        state.session_store.as_ref(),
        state.blob_store.as_ref(),
        &state.scaffold_root_dir,
        state.stream_tx.as_ref(),
        state.allow_scaffold,
    )
    .await;

    {
        let mut metrics = state.metrics.lock().await;
        metrics.tool_calls_executed += 1;
        if let Some(quote_val) = raw_val.get("quote")
            && let Ok(q) = serde_json::from_value::<RunQuote>(quote_val.clone())
        {
            metrics.prepared_quotes.push(q);
        }
    }

    let raw_str = match serde_json::to_string(&raw_val) {
        Ok(s) => s,
        Err(err) => return json_rpc_err(id, -32603, format!("encode tool result: {err}")),
    };

    let model_visible = match spill_tool_output_if_needed(
        state.blob_store.as_ref(),
        &state.session_id,
        &tc.id,
        &raw_str,
        state.spill_threshold_bytes,
    )
    .await
    {
        Ok(s) => s,
        Err(err) => return json_rpc_err(id, -32603, format!("spill tool result: {err}")),
    };

    if let Err(err) = state
        .session_store
        .append_event(
            &state.session_id,
            SessionEventKind::ToolResult,
            &model_visible,
            json!({
                "tool_call_id": tc.id,
                "tool_name": tc.name,
                "raw_bytes": raw_str.len(),
                "via": "mcp",
            }),
        )
        .await
    {
        return json_rpc_err(id, -32603, format!("persist ToolResult: {err}"));
    }

    if let Some(tx) = &state.stream_tx {
        let short_preview: String = model_visible.chars().take(240).collect();
        let _ = tx
            .send(AgentStreamEvent::ToolResult {
                id: tc.id.clone(),
                name: tc.name.clone(),
                preview: short_preview,
            })
            .await;
    }

    let is_error = raw_val.get("error").is_some()
        && raw_val.get("ok").and_then(Value::as_bool) != Some(true);

    json_rpc_ok(
        id,
        json!({
            "content": [{
                "type": "text",
                "text": model_visible,
            }],
            "isError": is_error,
        }),
    )
}
