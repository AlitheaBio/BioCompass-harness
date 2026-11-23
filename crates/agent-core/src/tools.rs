//! Strictly-scoped agent tool registry, execution port, and `BlobStore` large-payload spill engine.
//!
//! Architectural Invariant: `AgentExecutionPort` intentionally exposes NO `confirm_run` method.
//! Agents can only prepare a `RunQuote` via `prepare_plugin_run`; the trusted execution port applies any scientist-approved
//! spending allowance, outside the model tool interface.

use crate::AgentStreamEvent;
use crate::llm::{LlmToolCall, ToolDefinition};
use async_trait::async_trait;
use biocompass_core_traits::{
    BlobStore, CatalogId, CompassError, CompassResult, DatasetGrant, OrgId, PluginId,
    PrepareRunRequest, QueryAst, RunId, RunQuote, SessionId, SessionStore,
};
use biocompass_data_engine::{CatalogSummary, QueryTableResult};
use biocompass_node_runtime::InProcessNodeRuntime;
use biocompass_plugin_spec::{LoadedPlugin, TemplateKind, write_template_to_dir};
use serde_json::{Value, json};
use std::path::Path;
use tokio::sync::mpsc;

/// Default byte ceiling (`~12,000` tokens) before tool payloads spill into `BlobStore`.
pub const DEFAULT_SPILL_THRESHOLD_BYTES: usize = 48_000;

/// Result of applying a scientist-selected execution allowance to a signed quote.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentAdmission {
    pub execution_id: uuid::Uuid,
    pub run_id: RunId,
    pub status: AdmissionStatus,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionStatus {
    Submitted,
    ApprovalRequired,
    InsufficientCredits,
    AdmissionUnavailable,
}

/// Narrow capability port granted to `AgentRuntime` — physically excludes `confirm_run`!
#[async_trait]
pub trait AgentExecutionPort: Send + Sync {
    async fn list_plugins(&self) -> Vec<LoadedPlugin>;
    async fn list_catalog_manifests(&self) -> Vec<CatalogSummary>;
    async fn query_catalog(
        &self,
        id: &CatalogId,
        grant: &DatasetGrant,
        query: &QueryAst,
    ) -> CompassResult<QueryTableResult>;
    async fn prepare_plugin_quote(&self, req: PrepareRunRequest) -> CompassResult<(RunQuote, Option<AgentAdmission>)>;
}

#[async_trait]
impl AgentExecutionPort for InProcessNodeRuntime {
    async fn list_plugins(&self) -> Vec<LoadedPlugin> {
        InProcessNodeRuntime::list_plugins(self).await
    }

    async fn list_catalog_manifests(&self) -> Vec<CatalogSummary> {
        InProcessNodeRuntime::list_catalogs(self)
            .await
            .into_iter()
            .map(|c| c.manifest().clone())
            .collect()
    }

    async fn query_catalog(
        &self,
        id: &CatalogId,
        grant: &DatasetGrant,
        query: &QueryAst,
    ) -> CompassResult<QueryTableResult> {
        let cat = InProcessNodeRuntime::get_catalog(self, id).await?;
        cat.query(grant, query)
    }

    async fn prepare_plugin_quote(&self, req: PrepareRunRequest) -> CompassResult<(RunQuote, Option<AgentAdmission>)> {
        use biocompass_core_traits::RunDispatcher;
        Ok((self.prepare_run(req).await?, None))
    }
}

/// Return the exact 8 tools exposed to the LLM (`confirm_run` is strictly excluded).
pub fn allowed_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "list_catalogs".into(),
            description: "List registered DuckLake scientific catalogs, sealed snapshots, facets, and the active DatasetGrant policy.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "query_catalog".into(),
            description: "Execute a grant-scoped, SQL-free QueryAst over a registered 'rows'-level DuckLake catalog.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["catalog_id"],
                "properties": {
                    "catalog_id": { "type": "string" },
                    "table": { "type": "string", "description": "Optional table name (defaults to primary table)" },
                    "columns": { "type": "array", "items": { "type": "string" } },
                    "filter": { "type": "object", "description": "Typed FilterExpr node (op: eq|neq|in|lt|lte|gt|gte|is_null|is_not_null|starts_with|and|or)" },
                    "order_by": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["column"],
                            "properties": {
                                "column": { "type": "string" },
                                "direction": { "type": "string", "enum": ["asc", "desc"] }
                            }
                        }
                    },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000 },
                    "offset": { "type": "integer", "minimum": 0 },
                    "snapshot_id": { "type": "integer", "minimum": 0 }
                }
            }),
        },
        ToolDefinition {
            name: "prepare_plugin_run".into(),
            description: "Validate inputs and create a signed Froglet Quote for an OCI bio-compute plugin. The workbench applies the scientist-selected allowance and returns the decision.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["plugin_id", "params"],
                "properties": {
                    "plugin_id": { "type": "string" },
                    "plugin_version": { "type": "string", "minLength": 1, "description": "Exact published version. Omit to select the current available version." },
                    "offer_id": { "type": "string", "description": "Offer declared by the selected version." },
                    "params": { "type": "object" },
                    "attach_active_grant": { "type": "boolean", "default": true }
                }
            }),
        },
        ToolDefinition {
            name: "update_module_draft".into(),
            description: "Directly edit the live UI draft parameters or Jupyter notebook elements (cells, notebook_title, parameters, timeout_per_cell_sec, stop_on_error, kernel settings) of any module window without triggering a container execution quote. Use prepare_plugin_run instead when the user wants to execute immediately.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["plugin_id", "params"],
                "properties": {
                    "plugin_id": { "type": "string", "description": "Target plugin_id (e.g. 'alithea/bc-jupyter-notebook', 'alithea/hla-interactive-explorer')" },
                    "params": { "type": "object", "description": "Parameter fields or Jupyter cells/title/parameters to update in the open module UI" },
                    "merge": { "type": "boolean", "default": true, "description": "Merge keys into existing draft params (default true) or replace all params cleanly" },
                    "open_tab": { "type": "string", "enum": ["ui", "run", "results"], "description": "Optional tab to focus in the module window" }
                }
            }),
        },
        ToolDefinition {
            name: "list_runs".into(),
            description: "List recent plugin execution runs (including runs started from the UI or notebooks) with status, parameters summary, and artifact filenames.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "plugin_id": { "type": "string", "description": "Optional plugin_id filter (e.g. 'alithea/bc-jupyter-notebook')" },
                    "session_only": { "type": "boolean", "default": false, "description": "Filter to runs linked to the current chat session" },
                    "status": { "type": "string", "enum": ["prepared", "running", "succeeded", "failed", "canceled"] },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 }
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
            name: "get_run_artifact".into(),
            description: "Read the text/JSON/CSV/ipynb contents of an output artifact produced by a completed plugin run.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["run_id", "artifact_name"],
                "properties": {
                    "run_id": { "type": "string" },
                    "artifact_name": { "type": "string", "description": "Artifact filename from get_run_result (e.g. 'executed_notebook.ipynb', 'raw_data.csv')" }
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

/// Strip large inline base64 image payloads from `result_json` before sending to the LLM context.
fn redact_large_binary_blobs_for_llm(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                if (k == "image/png" || k == "image/jpeg" || k == "image/webp" || k == "image/svg+xml")
                    && let Some(s) = v.as_str()
                    && s.len() > 2048
                {
                    let omitted = format!("[{k} base64 omitted for LLM context, {} bytes]", s.len());
                    out.insert(k, Value::String(omitted));
                    continue;
                }
                out.insert(k, redact_large_binary_blobs_for_llm(v));
            }
            Value::Object(out)
        }
        Value::Array(arr) => {
            Value::Array(arr.into_iter().map(redact_large_binary_blobs_for_llm).collect())
        }
        other => other,
    }
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
    blob_store: &dyn BlobStore,
    scaffold_root: &Path,
    stream_tx: Option<&mpsc::Sender<AgentStreamEvent>>,
) -> Value {
    let outcome: CompassResult<Value> = async {
        match call.name.as_str() {
            "list_catalogs" => {
                let catalogs = port.list_catalog_manifests().await;
                Ok(json!({
                    "catalogs": catalogs,
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
                let mut query_obj = call.arguments.clone();
                if let Some(map) = query_obj.as_object_mut() {
                    map.remove("catalog_id");
                }
                let query: QueryAst = serde_json::from_value(query_obj)
                    .map_err(|e| CompassError::Validation(format!("invalid query_catalog args: {e}")))?;
                let res = port.query_catalog(&catalog_id, grant, &query).await?;
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
                let plugin_version: Option<String> =
                    serde_json::from_value(call.arguments["plugin_version"].clone()).map_err(|_| {
                        CompassError::Validation("plugin_version must be a string".into())
                    })?;
                biocompass_core_traits::validate_requested_plugin_version(plugin_version.as_deref())?;
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

                let offer_id: Option<String> =
                    serde_json::from_value(call.arguments["offer_id"].clone())
                        .map_err(|_| CompassError::Validation("offer_id must be a string".into()))?;
                let (quote, admission) = port
                    .prepare_plugin_quote(PrepareRunRequest {
                        org_id: org_id.clone(),
                        session_id: Some(session_id.clone()),
                        plugin_id,
                        plugin_version,
                        offer_id,
                        params,
                        dataset_grant: if attach_grant {
                            active_grant.cloned()
                        } else {
                            None
                        },
                    })
                    .await?;

                let needs_approval = admission.as_ref().is_none_or(|a| a.status == AdmissionStatus::ApprovalRequired);
                if let Some(tx) = stream_tx {
                    if let Some(decision) = &admission {
                        let _ = tx.send(AgentStreamEvent::AgentAdmission(decision.clone())).await;
                    }
                    if needs_approval {
                        let _ = tx.send(AgentStreamEvent::RunPrepared(Box::new(quote.clone()))).await;
                    }
                }
                let mut result = json!({ "status": "prepared_awaiting_user_confirmation", "quote": quote });
                if let Some(decision) = admission {
                    if !needs_approval {
                        result["status"] = json!(decision.status);
                    }
                    result["message"] = json!(decision.message);
                    result["execution_id"] = json!(decision.execution_id);
                }
                Ok(result)
            }
            "update_module_draft" => {
                let plugin_raw = call
                    .arguments
                    .get("plugin_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        CompassError::Validation("update_module_draft requires 'plugin_id'".into())
                    })?;
                let plugin_id = PluginId::new(plugin_raw).map_err(CompassError::Validation)?;
                let registered = port.list_plugins().await;
                if !registered.iter().any(|p| p.manifest.id == plugin_id) {
                    return Err(CompassError::NotFound(format!(
                        "plugin '{plugin_id}' is not registered"
                    )));
                }
                let params = call
                    .arguments
                    .get("params")
                    .cloned()
                    .ok_or_else(|| {
                        CompassError::Validation("update_module_draft requires 'params'".into())
                    })?;
                let Some(obj) = params.as_object() else {
                    return Err(CompassError::Validation(
                        "update_module_draft 'params' must be a JSON object".into(),
                    ));
                };
                let merge = call
                    .arguments
                    .get("merge")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let open_tab = call
                    .arguments
                    .get("open_tab")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| matches!(*s, "ui" | "run" | "results"))
                    .map(ToOwned::to_owned);
                let updated_keys: Vec<String> = obj.keys().cloned().collect();

                if let Some(tx) = stream_tx {
                    let _ = tx
                        .send(AgentStreamEvent::ModuleDraftUpdated {
                            plugin_id: plugin_id.as_str().to_owned(),
                            params,
                            merge,
                            open_tab: open_tab.clone(),
                        })
                        .await;
                }
                Ok(json!({
                    "status": "applied_to_open_module_ui",
                    "plugin_id": plugin_id.as_str(),
                    "merge": merge,
                    "open_tab": open_tab,
                    "updated_keys": updated_keys,
                }))
            }
            "list_runs" => {
                let plugin_filter = call
                    .arguments
                    .get("plugin_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let session_only = call
                    .arguments
                    .get("session_only")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let status_filter = call
                    .arguments
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let limit = call
                    .arguments
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(10)
                    .clamp(1, 50) as usize;

                let mut runs = session_store.list_runs(org_id).await?;
                runs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
                let filtered: Vec<Value> = runs
                    .into_iter()
                    .filter(|r| plugin_filter.is_none_or(|p| r.plugin_id.as_str() == p))
                    .filter(|r| !session_only || r.session_id.as_ref() == Some(session_id))
                    .filter(|r| {
                        status_filter.is_none_or(|s| {
                            serde_json::to_value(r.status)
                                .ok()
                                .and_then(|v| v.as_str().map(|st| st.eq_ignore_ascii_case(s)))
                                .unwrap_or(false)
                        })
                    })
                    .take(limit)
                    .map(|r| {
                        let summary = r
                            .result_json
                            .as_ref()
                            .and_then(|v| v.get("summary"))
                            .cloned();
                        json!({
                            "run_id": r.run_id,
                            "plugin_id": r.plugin_id,
                            "plugin_version": r.plugin_version,
                            "session_id": r.session_id,
                            "status": r.status,
                            "receipt_verified": r.receipt_verified,
                            "created_at": r.created_at,
                            "completed_at": r.completed_at,
                            "summary": summary,
                            "artifacts": r.artifacts.iter().map(|a| json!({
                                "name": a.name,
                                "media_type": a.media_type,
                                "size_bytes": a.size_bytes,
                            })).collect::<Vec<_>>(),
                            "error_message": r.error_message,
                        })
                    })
                    .collect();
                Ok(json!({ "runs": filtered }))
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
                let sanitized_result = run.result_json.map(redact_large_binary_blobs_for_llm);
                Ok(json!({
                    "run_id": run.run_id,
                    "plugin_id": run.plugin_id,
                    "status": run.status,
                    "params": run.params,
                    "receipt_verified": run.receipt_verified,
                    "result_json": sanitized_result,
                    "artifacts": run.artifacts,
                    "error_message": run.error_message,
                }))
            }
            "get_run_artifact" => {
                let run_raw = call
                    .arguments
                    .get("run_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        CompassError::Validation("get_run_artifact requires 'run_id'".into())
                    })?;
                let artifact_name = call
                    .arguments
                    .get("artifact_name")
                    .and_then(Value::as_str)
                    .map(|s| s.trim_start_matches('/'))
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        CompassError::Validation("get_run_artifact requires 'artifact_name'".into())
                    })?;
                let run_id = RunId::new(run_raw).map_err(CompassError::Validation)?;
                let run = session_store.get_run(&run_id).await?;
                if &run.org_id != org_id {
                    return Err(CompassError::AccessDenied(
                        "run belongs to a different organization".into(),
                    ));
                }
                let art = run
                    .artifacts
                    .iter()
                    .find(|a| a.name == artifact_name)
                    .ok_or_else(|| {
                        CompassError::NotFound(format!(
                            "artifact '{artifact_name}' not found on run '{run_id}'"
                        ))
                    })?;
                let bytes = blob_store.get_bytes(&art.blob_key).await?;
                if art.media_type.starts_with("image/") && !art.media_type.contains("svg") {
                    return Ok(json!({
                        "run_id": run.run_id,
                        "artifact_name": art.name,
                        "media_type": art.media_type,
                        "size_bytes": art.size_bytes,
                        "sha256": art.sha256,
                        "note": "Binary image artifact; inspect visual rendering in the module UI or download via /api/v1/runs/:run_id/artifacts/:name."
                    }));
                }
                let text = String::from_utf8(bytes).map_err(|_| {
                    CompassError::Validation(format!(
                        "artifact '{}' ({}) is binary and cannot be read as UTF-8 text",
                        art.name, art.media_type
                    ))
                })?;
                Ok(json!({
                    "run_id": run.run_id,
                    "artifact_name": art.name,
                    "media_type": art.media_type,
                    "size_bytes": art.size_bytes,
                    "content": text,
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
