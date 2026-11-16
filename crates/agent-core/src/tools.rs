//! Strictly-scoped agent tool registry, execution port, and `BlobStore` large-payload spill engine.
//!
//! Architectural Invariant: `AgentExecutionPort` intentionally exposes NO `confirm_run` method.
//! Agents can only prepare a `RunQuote` via `prepare_plugin_run`; confirming a `Deal` and spending
//! ACT credits requires explicit user/API confirmation outside `biocompass-agent-core`.

use crate::AgentStreamEvent;
use crate::llm::{LlmToolCall, ToolDefinition};
use async_trait::async_trait;
use biocompass_core_traits::{
    BlobStore, CatalogId, CompassError, CompassResult, DatasetGrant, OrgId, PluginId,
    PrepareRunRequest, RunId, RunQuote, SessionId, SessionStore,
};
use biocompass_data_engine::{CatalogQuery, DuckDbCatalogEngine};
use biocompass_node_runtime::InProcessNodeRuntime;
use biocompass_plugin_spec::{LoadedPlugin, TemplateKind, write_template_to_dir};
use serde_json::{Value, json};
use std::path::Path;
use tokio::sync::mpsc;

/// Default byte ceiling (`~12,000` tokens) before tool payloads spill into `BlobStore`.
pub const DEFAULT_SPILL_THRESHOLD_BYTES: usize = 48_000;

/// Narrow capability port granted to `AgentRuntime` — physically excludes `confirm_run`!
#[async_trait]
pub trait AgentExecutionPort: Send + Sync {
    async fn list_plugins(&self) -> Vec<LoadedPlugin>;
    async fn list_catalogs(&self) -> Vec<DuckDbCatalogEngine>;
    async fn get_catalog(&self, id: &CatalogId) -> CompassResult<DuckDbCatalogEngine>;
    async fn prepare_plugin_quote(&self, req: PrepareRunRequest) -> CompassResult<RunQuote>;
}

#[async_trait]
impl AgentExecutionPort for InProcessNodeRuntime {
    async fn list_plugins(&self) -> Vec<LoadedPlugin> {
        InProcessNodeRuntime::list_plugins(self).await
    }

    async fn list_catalogs(&self) -> Vec<DuckDbCatalogEngine> {
        InProcessNodeRuntime::list_catalogs(self).await
    }

    async fn get_catalog(&self, id: &CatalogId) -> CompassResult<DuckDbCatalogEngine> {
        InProcessNodeRuntime::get_catalog(self, id).await
    }

    async fn prepare_plugin_quote(&self, req: PrepareRunRequest) -> CompassResult<RunQuote> {
        use biocompass_core_traits::RunDispatcher;
        self.prepare_run(req).await
    }
}

/// Return the exact 5 tools exposed to the LLM (`confirm_run` is strictly excluded).
pub fn allowed_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "list_catalogs".into(),
            description: "List registered scientific Parquet catalogs and the active DatasetGrant policy.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "query_catalog".into(),
            description: "Execute a grant-scoped, SQL-injection-safe DuckDB query over a registered Parquet catalog.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["catalog_id"],
                "properties": {
                    "catalog_id": { "type": "string" },
                    "organs": { "type": "array", "items": { "type": "string" } },
                    "conditions": { "type": "array", "items": { "type": "string" } },
                    "hla_alleles": { "type": "array", "items": { "type": "string" } },
                    "gene_symbols": { "type": "array", "items": { "type": "string" } },
                    "min_length": { "type": "integer" },
                    "max_length": { "type": "integer" },
                    "max_affinity_nm": { "type": "number" },
                    "sequence_prefix": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000 }
                }
            }),
        },
        ToolDefinition {
            name: "prepare_plugin_run".into(),
            description: "Validate inputs and create a signed Froglet Quote for an OCI bio-compute plugin. Requires explicit user confirmation before container execution.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["plugin_id", "params"],
                "properties": {
                    "plugin_id": { "type": "string" },
                    "params": { "type": "object" },
                    "attach_active_grant": { "type": "boolean", "default": true }
                }
            }),
        },
        ToolDefinition {
            name: "get_run_result".into(),
            description: "Retrieve execution status, structured result.json, artifact metadata, and Froglet Schnorr receipt verification status for a run_id.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["run_id"],
                "properties": {
                    "run_id": { "type": "string" }
                }
            }),
        },
        ToolDefinition {
            name: "scaffold_plugin".into(),
            description: "Scaffold a new OCI container plugin directory from one of the 4 starter templates (python-uv, r-bioc, nextflow, rust-cli).".into(),
            input_schema: json!({
                "type": "object",
                "required": ["template", "plugin_id", "display_name"],
                "properties": {
                    "template": {
                        "type": "string",
                        "enum": ["python-uv", "r-bioc", "nextflow", "rust-cli"]
                    },
                    "plugin_id": { "type": "string", "description": "Namespaced id '<org>/<slug>'" },
                    "display_name": { "type": "string" },
                    "include_vite_ui": { "type": "boolean", "default": false }
                }
            }),
        },
    ]
}

/// Spill tool output JSON into `BlobStore` when larger than `threshold_bytes`, returning a `< 2 KB`
/// model-context envelope with `blob_key`, `sha256`, `original_bytes`, and a safe UTF-8 preview.
pub async fn spill_tool_output_if_needed(
    blob_store: &dyn BlobStore,
    session_id: &SessionId,
    tool_call_id: &str,
    raw_output: &str,
    threshold_bytes: usize,
) -> CompassResult<String> {
    if raw_output.len() <= threshold_bytes {
        return Ok(raw_output.to_owned());
    }

    let safe_call_id: String = tool_call_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let blob_key = format!("spills/{}/{}.json", session_id.as_str(), safe_call_id);
    let sha256 = blob_store.put_bytes(&blob_key, raw_output.as_bytes()).await?;

    let preview: String = raw_output.chars().take(750).collect();
    let envelope = json!({
        "spilled_to_blob_store": true,
        "blob_key": blob_key,
        "sha256": sha256,
        "original_bytes": raw_output.len(),
        "preview": preview,
        "note": "Full tool output exceeded context budget and was persisted cleanly to BlobStore."
    });
    serde_json::to_string(&envelope)
        .map_err(|e| CompassError::Internal(format!("serialize spill envelope: {e}")))
}

/// Execute one validated tool call on behalf of the agent loop.
#[allow(clippy::too_many_arguments)]
pub async fn execute_agent_tool(
    call: &LlmToolCall,
    org_id: &OrgId,
    session_id: &SessionId,
    active_grant: Option<&DatasetGrant>,
    port: &dyn AgentExecutionPort,
    session_store: &dyn SessionStore,
    scaffold_root: &Path,
    stream_tx: Option<&mpsc::Sender<AgentStreamEvent>>,
) -> Value {
    let outcome: CompassResult<Value> = async {
        match call.name.as_str() {
            "list_catalogs" => {
                let catalogs = port.list_catalogs().await;
                let items: Vec<Value> = catalogs
                    .iter()
                    .map(|c| serde_json::to_value(c.manifest()).unwrap_or(Value::Null))
                    .collect();
                Ok(json!({
                    "catalogs": items,
                    "active_grant": active_grant,
                }))
            }
            "query_catalog" => {
                let grant = active_grant.ok_or_else(|| {
                    CompassError::AccessDenied(
                        "query_catalog requires an active DatasetGrant on the session".into(),
                    )
                })?;
                let catalog_id_raw = call
                    .arguments
                    .get("catalog_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        CompassError::Validation("query_catalog requires 'catalog_id'".into())
                    })?;
                let catalog_id =
                    CatalogId::new(catalog_id_raw).map_err(CompassError::Validation)?;
                if grant.catalog_id != catalog_id {
                    return Err(CompassError::AccessDenied(format!(
                        "active grant '{}' only permits catalog '{}', not '{}'",
                        grant.grant_id, grant.catalog_id, catalog_id
                    )));
                }
                let catalog = port.get_catalog(&catalog_id).await?;
                let mut query_obj = call.arguments.clone();
                if let Some(map) = query_obj.as_object_mut() {
                    map.remove("catalog_id");
                }
                let query: CatalogQuery = serde_json::from_value(query_obj)
                    .map_err(|e| CompassError::Validation(format!("invalid query_catalog args: {e}")))?;
                let res = catalog.query(grant, &query)?;
                serde_json::to_value(res)
                    .map_err(|e| CompassError::Internal(format!("serialize query result: {e}")))
            }
            "prepare_plugin_run" => {
                let plugin_raw = call
                    .arguments
                    .get("plugin_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        CompassError::Validation("prepare_plugin_run requires 'plugin_id'".into())
                    })?;
                let plugin_id = PluginId::new(plugin_raw).map_err(CompassError::Validation)?;
                let params = call
                    .arguments
                    .get("params")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let attach_grant = call
                    .arguments
                    .get("attach_active_grant")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);

                let quote = port
                    .prepare_plugin_quote(PrepareRunRequest {
                        org_id: org_id.clone(),
                        session_id: Some(session_id.clone()),
                        plugin_id,
                        params,
                        dataset_grant: if attach_grant {
                            active_grant.cloned()
                        } else {
                            None
                        },
                    })
                    .await?;

                if let Some(tx) = stream_tx {
                    let _ = tx.send(AgentStreamEvent::RunPrepared(quote.clone())).await;
                }
                Ok(json!({
                    "status": "prepared_awaiting_user_confirmation",
                    "quote": quote,
                }))
            }
            "get_run_result" => {
                let run_raw = call
                    .arguments
                    .get("run_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        CompassError::Validation("get_run_result requires 'run_id'".into())
                    })?;
                let run_id = RunId::new(run_raw).map_err(CompassError::Validation)?;
                let run = session_store.get_run(&run_id).await?;
                if &run.org_id != org_id {
                    return Err(CompassError::AccessDenied(
                        "run belongs to a different organization".into(),
                    ));
                }
                Ok(json!({
                    "run_id": run.run_id,
                    "status": run.status,
                    "receipt_verified": run.receipt_verified,
                    "result_json": run.result_json,
                    "artifacts": run.artifacts,
                    "error_message": run.error_message,
                }))
            }
            "scaffold_plugin" => {
                let template_raw = call
                    .arguments
                    .get("template")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        CompassError::Validation("scaffold_plugin requires 'template'".into())
                    })?;
                let plugin_id = call
                    .arguments
                    .get("plugin_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        CompassError::Validation("scaffold_plugin requires 'plugin_id'".into())
                    })?;
                let display_name = call
                    .arguments
                    .get("display_name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        CompassError::Validation("scaffold_plugin requires 'display_name'".into())
                    })?;
                let include_ui = call
                    .arguments
                    .get("include_vite_ui")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);

                let kind = TemplateKind::parse(template_raw)?;
                let slug = plugin_id
                    .split('/')
                    .nth(1)
                    .filter(|s| {
                        !s.is_empty()
                            && s.chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                    })
                    .ok_or_else(|| {
                        CompassError::Validation(
                            "plugin_id slug must contain only [A-Za-z0-9_-]".into(),
                        )
                    })?;
                let target_dir = scaffold_root.join(slug);
                let created =
                    write_template_to_dir(kind, plugin_id, display_name, include_ui, &target_dir)?;
                Ok(json!({
                    "plugin_id": plugin_id,
                    "template": kind.as_str(),
                    "directory": target_dir,
                    "files_written": created,
                }))
            }
            forbidden if forbidden.contains("confirm") => Err(CompassError::AccessDenied(
                "CONSENT_GATE_ENFORCED: agents cannot confirm runs or spend ACT credits; only prepare_plugin_run is permitted".into(),
            )),
            other => Err(CompassError::Validation(format!(
                "unknown agent tool '{other}'"
            ))),
        }
    }
    .await;

    match outcome {
        Ok(val) => val,
        Err(err) => json!({
            "error": true,
            "message": err.to_string(),
        }),
    }
}
