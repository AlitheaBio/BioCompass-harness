//! Ephemeral per-turn loopback Streamable HTTP MCP server (`127.0.0.1:0/mcp/<token>`).
//!
//! Thin loopback adapter delegating JSON-RPC 2.0 framing, protocol version negotiation,
//! and method dispatch to `biocompass-mcp-core`, while exposing `allowed_tool_definitions_for(...)`
//! strictly through `execute_agent_tool_with_options(...)`, preserving:
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
use async_trait::async_trait;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use biocompass_core_traits::{
    BlobStore, CompassError, CompassResult, DatasetGrant, OrgId, RunQuote, SessionEventKind,
    SessionId, SessionStore,
};
use biocompass_mcp_core::{
    CallToolResult, DispatchOutcome, JsonRpcErrorObject, JsonRpcRequest, McpErrorCode, ServerInfo,
    ToolAnnotations, ToolDefinition, ToolHost, dispatch_jsonrpc,
};
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

#[async_trait]
impl ToolHost for McpAppState {
    fn server_info(&self) -> ServerInfo {
        ServerInfo {
            name: "biocompass-mcp".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    async fn list_tools(&self) -> Result<Vec<ToolDefinition>, JsonRpcErrorObject> {
        let tools = allowed_tool_definitions_for(self.allow_scaffold)
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
                ToolDefinition {
                    name: t.name,
                    description: t.description,
                    input_schema: t.input_schema,
                    annotations: Some(if read_only {
                        ToolAnnotations::read_only()
                    } else {
                        ToolAnnotations::mutating(false)
                    }),
                }
            })
            .collect();
        Ok(tools)
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<CallToolResult, JsonRpcErrorObject> {
        if !allowed_tool_definitions_for(self.allow_scaffold)
            .iter()
            .any(|t| t.name == name)
        {
            let err_text = serde_json::to_string(&json!({
                "error": format!("tool '{name}' is not allowed or requires explicit user consent")
            }))
            .unwrap_or_default();
            return Ok(CallToolResult::error_text(err_text));
        }

        let call_id = format!("mcp_{}", Uuid::new_v4().simple());
        let tc = LlmToolCall {
            id: call_id,
            name: name.to_owned(),
            arguments,
        };

        self.session_store
            .append_event(
                &self.session_id,
                SessionEventKind::ToolCall,
                &tc.name,
                json!({
                    "id": tc.id,
                    "arguments": tc.arguments,
                    "via": "mcp",
                }),
            )
            .await
            .map_err(|err| {
                JsonRpcErrorObject::new(
                    McpErrorCode::InternalError,
                    format!("persist ToolCall: {err}"),
                )
            })?;

        if let Some(tx) = &self.stream_tx {
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
            &self.org_id,
            &self.session_id,
            self.active_grant.as_ref(),
            self.execution_port.as_ref(),
            self.session_store.as_ref(),
            self.blob_store.as_ref(),
            &self.scaffold_root_dir,
            self.stream_tx.as_ref(),
            self.allow_scaffold,
        )
        .await;

        {
            let mut metrics = self.metrics.lock().await;
            metrics.tool_calls_executed += 1;
            if let Some(quote_val) = raw_val.get("quote")
                && let Ok(q) = serde_json::from_value::<RunQuote>(quote_val.clone())
            {
                metrics.prepared_quotes.push(q);
            }
        }

        let raw_str = serde_json::to_string(&raw_val).map_err(|err| {
            JsonRpcErrorObject::new(
                McpErrorCode::InternalError,
                format!("encode tool result: {err}"),
            )
        })?;

        let model_visible = spill_tool_output_if_needed(
            self.blob_store.as_ref(),
            &self.session_id,
            &tc.id,
            &raw_str,
            self.spill_threshold_bytes,
        )
        .await
        .map_err(|err| {
            JsonRpcErrorObject::new(
                McpErrorCode::InternalError,
                format!("spill tool result: {err}"),
            )
        })?;

        self.session_store
            .append_event(
                &self.session_id,
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
            .map_err(|err| {
                JsonRpcErrorObject::new(
                    McpErrorCode::InternalError,
                    format!("persist ToolResult: {err}"),
                )
            })?;

        if let Some(tx) = &self.stream_tx {
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

        Ok(if is_error {
            CallToolResult::error_text(model_visible)
        } else {
            CallToolResult::text(model_visible)
        })
    }
}

async fn handle_mcp_post(
    State(state): State<McpAppState>,
    AxumPath(token): AxumPath<String>,
    Json(req): Json<JsonRpcRequest>,
) -> Response {
    if token != state.token {
        return StatusCode::NOT_FOUND.into_response();
    }

    match dispatch_jsonrpc(&state, req).await {
        DispatchOutcome::Notification => StatusCode::ACCEPTED.into_response(),
        DispatchOutcome::Response { response, .. } => (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )],
            Json(response),
        )
            .into_response(),
    }
}
