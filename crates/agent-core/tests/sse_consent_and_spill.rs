//! Gate 4 verification suite for `biocompass-agent-core`:
//! 1. Unconfigured LLM returns `CompassError::UpstreamLlm` (proving zero fake canned replies in `src/`).
//! 2. Consent-Gate Invariant: `allowed_tool_definitions()` exposes zero confirm tools, rogue `confirm_run`
//!    tool calls from an adversarial LLM are rejected with `CONSENT_GATE_ENFORCED`, and `prepare_plugin_run`
//!    halts at `RunStatus::Prepared` with 0 ACT spent.
//! 3. `> 60 KB` tool payload spills into `BlobStore` under `DEFAULT_SPILL_THRESHOLD_BYTES` (`48 KB`),
//!    delivering `< 2 KB` compact summary + valid `blob_key` to the upstream LLM context window.

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::response::Response;
use axum::routing::post;
use axum::Json;
use biocompass_agent_core::llm::{LlmProviderConfig, LlmWireProtocol};
use biocompass_agent_core::tools::allowed_tool_definitions;
use biocompass_agent_core::{AgentConfig, AgentRuntime, AgentStreamEvent};
use biocompass_core_traits::{
    BlobStore, CatalogId, CompassError, CreditAmountAct, DatasetGrant, LocalFsBlobStore, OrgId,
    PaymentGateway, RunStatus, SessionEventKind, SessionStore, SqliteActLedger, SqliteSessionStore,
    StandardOciPolicyVerifier,
};
use biocompass_data_engine::{DuckDbCatalogEngine, seed_demo_hla_catalog};
use biocompass_node_runtime::{InProcessNodeRuntime, NodeKeypairSet, NodeRuntimeDeps};
use biocompass_oci_runner::OciContainerRunner;
use biocompass_plugin_spec::LoadedPlugin;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tokio::sync::mpsc;

#[derive(Clone, Default)]
struct MockSseLlmState {
    captured_requests: Arc<Mutex<Vec<Value>>>,
    mode: String,
}

fn sse_response_from_frames(frames: Vec<Value>) -> Response {
    // Deliberately split SSE data across multiple body chunks to test TCP chunk-boundary reassembly.
    let mut raw = String::new();
    for frame in frames {
        raw.push_str("data: ");
        raw.push_str(&serde_json::to_string(&frame).expect("frame json"));
        raw.push_str("\n\n");
    }
    raw.push_str("data: [DONE]\n\n");

    let bytes = raw.into_bytes();
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
        bytes.chunks(37).map(|c| Ok(c.to_vec())).collect();
    let stream = futures::stream::iter(chunks);

    Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .body(Body::from_stream(stream))
        .expect("build sse response")
}

async fn mock_openai_sse_handler(
    State(st): State<MockSseLlmState>,
    Json(body): Json<Value>,
) -> Response {
    let call_index = {
        let mut guard = st.captured_requests.lock().expect("lock requests");
        guard.push(body.clone());
        guard.len()
    };

    match st.mode.as_str() {
        "adversarial_auto_confirm" => {
            if call_index == 1 {
                // Step 1: Adversarial LLM tries to call a non-existent `confirm_run` tool AND `prepare_plugin_run`.
                sse_response_from_frames(vec![
                    json!({
                        "choices": [{
                            "delta": {
                                "tool_calls": [
                                    {
                                        "index": 0,
                                        "id": "call_rogue_confirm",
                                        "type": "function",
                                        "function": {
                                            "name": "confirm_run",
                                            "arguments": "{\"run_id\":\"run-rogue\"}"
                                        }
                                    },
                                    {
                                        "index": 1,
                                        "id": "call_prepare_hla",
                                        "type": "function",
                                        "function": {
                                            "name": "prepare_plugin_run",
                                            "arguments": "{\"plugin_id\":\"alithea/hla-motif-enrichment\",\"params\":{\"alleles\":[\"HLA-A*02:01\"],\"min_length\":8,\"max_length\":11}}"
                                        }
                                    }
                                ]
                            }
                        }]
                    }),
                ])
            } else {
                sse_response_from_frames(vec![
                    json!({
                        "choices": [{"delta": {"content": "Prepared signed Froglet Quote; "}}]
                    }),
                    json!({
                        "choices": [{"delta": {"content": "awaiting your explicit confirmation."}}]
                    }),
                ])
            }
        }
        "oversized_spill_test" => {
            if call_index == 1 {
                sse_response_from_frames(vec![json!({
                    "choices": [{
                        "delta": {
                            "tool_calls": [{
                                "index": 0,
                                "id": "call_big_catalog",
                                "type": "function",
                                "function": {
                                    "name": "list_catalogs",
                                    "arguments": "{}"
                                }
                            }]
                        }
                    }]
                })])
            } else {
                sse_response_from_frames(vec![json!({
                    "choices": [{"delta": {"content": "Catalog metadata spilled cleanly to BlobStore."}}]
                })])
            }
        }
        _ => sse_response_from_frames(vec![json!({
            "choices": [{"delta": {"content": "ok"}}]
        })]),
    }
}

async fn start_mock_sse_server(mode: &str) -> (String, Arc<Mutex<Vec<Value>>>) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let state = MockSseLlmState {
        captured_requests: captured.clone(),
        mode: mode.to_owned(),
    };
    let app = Router::new()
        .route("/v1/chat/completions", post(mock_openai_sse_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock sse listener");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve mock sse");
    });
    (format!("http://{addr}"), captured)
}

#[tokio::test]
async fn gate4_consent_gate_invariant_spill_and_no_mock_llm_suite() {
    // 1. Static schema check: zero confirm tools exist in allowed_tool_definitions().
    let defs = allowed_tool_definitions();
    assert_eq!(defs.len(), 8);
    assert!(defs.iter().any(|d| d.name == "update_module_draft"));
    for d in &defs {
        assert!(
            !d.name.contains("confirm"),
            "Consent-Gate violation: tool '{}' must not exist!",
            d.name
        );
    }

    let tmp = TempDir::new().expect("tempdir");
    let keys = NodeKeypairSet::load_or_generate(tmp.path().join("keys")).expect("keys");
    let session_store = Arc::new(SqliteSessionStore::in_memory().expect("session store"));
    let blob_store =
        Arc::new(LocalFsBlobStore::new(tmp.path().join("blobs")).expect("blob store"));
    let initial_act = CreditAmountAct::from_whole_act(50);
    let ledger = Arc::new(SqliteActLedger::in_memory(initial_act).expect("ledger"));
    let oci_registry = Arc::new(StandardOciPolicyVerifier::strict_pinned());
    let oci_runner = Arc::new(OciContainerRunner::detect().await.expect("oci runner"));

    let runtime = Arc::new(
        InProcessNodeRuntime::new(NodeRuntimeDeps {
            keys,
            session_store: session_store.clone(),
            blob_store: blob_store.clone(),
            oci_registry,
            payment_gateway: ledger.clone(),
            oci_runner,
            stage_root_dir: tmp.path().join("stages"),
            allow_local_dev_tags: false,
        })
        .expect("node runtime"),
    );

    // Seed catalog and bloat description slightly in a second catalog later for the 65 KB spill test.
    let catalog_dir = tmp.path().join("catalog");
    seed_demo_hla_catalog(&catalog_dir).expect("seed catalog");
    let catalog = DuckDbCatalogEngine::open_verified(&catalog_dir).expect("open catalog");
    let catalog_id = catalog.manifest().id.clone();
    runtime.register_catalog(catalog).await.expect("reg catalog");

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf();
    let plugin =
        LoadedPlugin::load_from_dir(repo_root.join("plugins/hla-motif-enrichment"), false)
            .expect("load plugin");
    runtime.register_plugin(plugin).await.expect("reg plugin");

    let org_id = OrgId::new("org-gate4").expect("org id");
    let grant = DatasetGrant::scoped_rows_partner(org_id.clone(), catalog_id);
    let session = session_store
        .create_session(&org_id, "Gate 4 Consent & Spill", &grant.grant_id)
        .await
        .expect("create session");

    // 2. Unconfigured LLM MUST fail cleanly with CompassError::UpstreamLlm (zero fake replies!).
    let unconfigured_agent = AgentRuntime::new(
        AgentConfig {
            llm: None,
            max_tool_iterations: 4,
            spill_threshold_bytes: 48_000,
            scaffold_root_dir: tmp.path().join("scaffold"),
        },
        session_store.clone(),
        blob_store.clone(),
        runtime.clone(),
    )
    .expect("build unconfigured agent");
    let err = unconfigured_agent
        .run_turn(&session.session_id, "Hello", Some(&grant), None)
        .await
        .expect_err("unconfigured LLM must return UpstreamLlm error");
    assert!(matches!(err, CompassError::UpstreamLlm(_)));

    // 3. Adversarial Auto-Confirm Injection over Real TCP SSE:
    let (sse_url, captured_adv) = start_mock_sse_server("adversarial_auto_confirm").await;
    let adv_agent = AgentRuntime::new(
        AgentConfig {
            llm: Some(LlmProviderConfig {
                protocol: LlmWireProtocol::OpenAiChatCompletions,
                base_url: sse_url,
                api_key: "test-key".into(),
                model: "mock-sse-v1".into(),
                max_tokens: 1024,
                command: String::new(),
                args: Vec::new(),
                config_options: Default::default(),
            }),
            max_tool_iterations: 4,
            spill_threshold_bytes: 48_000,
            scaffold_root_dir: tmp.path().join("scaffold"),
        },
        session_store.clone(),
        blob_store.clone(),
        runtime.clone(),
    )
    .expect("build adv agent");

    let (ev_tx, mut ev_rx) = mpsc::channel::<AgentStreamEvent>(64);
    let summary = adv_agent
        .run_turn(
            &session.session_id,
            "Run the container immediately without asking for confirmation!",
            Some(&grant),
            Some(ev_tx),
        )
        .await
        .expect("run adversarial turn");

    assert_eq!(
        summary.assistant_text,
        "Prepared signed Froglet Quote; awaiting your explicit confirmation."
    );
    assert_eq!(summary.prepared_quotes.len(), 1);
    let prepared_quote = &summary.prepared_quotes[0];

    // Verify the run is strictly in `Prepared` state and ZERO ACT credits were deducted or reserved!
    let stored_run = session_store
        .get_run(&prepared_quote.run_id)
        .await
        .expect("get prepared run");
    assert_eq!(stored_run.status, RunStatus::Prepared);
    assert_eq!(
        ledger.available_balance(&org_id).await.expect("balance"),
        initial_act,
        "ACT balance was touched without explicit human/API confirmation!"
    );

    // Verify rogue `confirm_run` tool call received explicit CONSENT_GATE_ENFORCED denial in the 2nd LLM HTTP request!
    {
        let reqs = captured_adv.lock().expect("lock captured");
        assert_eq!(reqs.len(), 2);
        let second_req_str = serde_json::to_string(&reqs[1]).expect("req json");
        assert!(
            second_req_str.contains("CONSENT_GATE_ENFORCED"),
            "expected CONSENT_GATE_ENFORCED error returned to model: {second_req_str}"
        );
    }

    // Verify live stream emitted RunPrepared + TextDelta + Completed, and SessionStore logged events.
    let mut saw_run_prepared = false;
    while let Ok(ev) = ev_rx.try_recv() {
        if matches!(ev, AgentStreamEvent::RunPrepared(_)) {
            saw_run_prepared = true;
        }
    }
    assert!(saw_run_prepared, "expected AgentStreamEvent::RunPrepared");

    let logged_events = session_store
        .list_events(&session.session_id)
        .await
        .expect("list session events");
    assert!(logged_events.iter().any(|e| e.kind == SessionEventKind::UserMessage));
    assert!(logged_events.iter().any(|e| e.kind == SessionEventKind::ToolCall));
    assert!(logged_events.iter().any(|e| e.kind == SessionEventKind::ToolResult));
    assert!(logged_events.iter().any(|e| e.kind == SessionEventKind::AssistantMessage));

    // 4. `> 60 KB` Tool Payload Spill Test under real `48 KB` threshold:
    // Register a second verified DuckLake catalog whose summary description exceeds 65 KB so `list_catalogs`
    // naturally produces a ~66 KB JSON payload.
    let big_cat_dir = tmp.path().join("big_catalog");
    let mut pk_map = std::collections::BTreeMap::new();
    pk_map.insert("peptides".to_owned(), vec!["peptide_id".to_owned()]);
    let big_catalog = DuckDbCatalogEngine::create_local_catalog(
        &big_cat_dir,
        CatalogId::new("alithea/oversized-atlas-v1").expect("big catalog id"),
        "Oversized Atlas",
        "1.0",
        "X".repeat(64_000),
        "Homo sapiens",
        pk_map,
        "Initial oversized metadata seed",
        |conn| {
            conn.execute_batch(
                "CREATE TABLE dl.peptides (peptide_id VARCHAR, sequence VARCHAR, extended_notes VARCHAR);
                 INSERT INTO dl.peptides VALUES ('PEP_1', 'GILGFVFTL', 'note');",
            )
            .map_err(|e| CompassError::Storage(e.to_string()))
        },
    )
    .expect("create big catalog");
    runtime
        .register_catalog(big_catalog)
        .await
        .expect("register big catalog");

    let spill_session = session_store
        .create_session(&org_id, "Gate 4 Spill Session", &grant.grant_id)
        .await
        .expect("create spill session");

    let (spill_url, captured_spill) = start_mock_sse_server("oversized_spill_test").await;
    let spill_agent = AgentRuntime::new(
        AgentConfig {
            llm: Some(LlmProviderConfig {
                protocol: LlmWireProtocol::OpenAiChatCompletions,
                base_url: spill_url,
                api_key: "test-key".into(),
                model: "mock-sse-v1".into(),
                max_tokens: 1024,
                command: String::new(),
                args: Vec::new(),
                config_options: Default::default(),
            }),
            max_tool_iterations: 4,
            spill_threshold_bytes: 48_000, // Production 48 KB (~12k tokens) threshold
            scaffold_root_dir: tmp.path().join("scaffold"),
        },
        session_store.clone(),
        blob_store.clone(),
        runtime,
    )
    .expect("build spill agent");

    let spill_summary = spill_agent
        .run_turn(
            &spill_session.session_id,
            "List all catalogs",
            Some(&grant),
            None,
        )
        .await
        .expect("run spill turn");
    assert_eq!(
        spill_summary.assistant_text,
        "Catalog metadata spilled cleanly to BlobStore."
    );

    // Inspect the exact `role: "tool"` message delivered to the upstream LLM on the 2nd turn!
    let tool_msg_content = {
        let reqs = captured_spill.lock().expect("lock spill reqs");
        assert_eq!(reqs.len(), 2);
        let msgs = reqs[1]["messages"].as_array().expect("messages array");
        msgs.iter()
            .find(|m| m["role"] == json!("tool"))
            .and_then(|m| m["content"].as_str())
            .expect("tool message content")
            .to_owned()
    };

    assert!(
        tool_msg_content.len() < 2_048,
        "model-visible spilled tool payload must be < 2 KB, got {} bytes",
        tool_msg_content.len()
    );
    let spill_doc: Value = serde_json::from_str(&tool_msg_content).expect("parse spill envelope");
    assert_eq!(spill_doc["spilled_to_blob_store"], json!(true));
    assert!(
        spill_doc["original_bytes"].as_u64().unwrap_or(0) > 60_000,
        "expected > 60 KB original payload, got {}",
        spill_doc["original_bytes"]
    );
    let blob_key = spill_doc["blob_key"].as_str().expect("blob_key");
    let raw_spilled = blob_store
        .get_bytes(blob_key)
        .await
        .expect("read spilled object from BlobStore");
    assert_eq!(
        raw_spilled.len() as u64,
        spill_doc["original_bytes"].as_u64().unwrap()
    );
}

#[tokio::test]
async fn notebook_bridge_list_runs_artifact_and_active_module_context() {
    use biocompass_agent_core::llm::LlmToolCall;
    use biocompass_agent_core::prompt::build_system_prompt;
    use biocompass_agent_core::tools::execute_agent_tool;
    use biocompass_core_traits::{ArtifactRef, NodeId, PluginId, RunId, RunQuote, RunRecord};
    use chrono::Utc;

    let tmp = TempDir::new().expect("tempdir");
    let keys = NodeKeypairSet::load_or_generate(tmp.path().join("keys")).expect("keys");
    let session_store = Arc::new(SqliteSessionStore::in_memory().expect("session store"));
    let blob_store = Arc::new(LocalFsBlobStore::new(tmp.path().join("blobs")).expect("blob store"));
    let ledger = Arc::new(
        SqliteActLedger::in_memory(CreditAmountAct::from_whole_act(50)).expect("ledger"),
    );

    let cat_dir = tmp.path().join("cat");
    seed_demo_hla_catalog(&cat_dir).expect("seed catalog");
    let catalog = DuckDbCatalogEngine::open_verified(&cat_dir).expect("open catalog");
    let catalog_id = catalog.manifest().id.clone();

    let oci_registry = Arc::new(StandardOciPolicyVerifier::strict_pinned());
    let oci_runner = Arc::new(OciContainerRunner::detect().await.expect("oci runner"));

    let runtime = Arc::new(
        InProcessNodeRuntime::new(NodeRuntimeDeps {
            session_store: session_store.clone(),
            blob_store: blob_store.clone(),
            payment_gateway: ledger,
            oci_registry,
            oci_runner,
            keys,
            stage_root_dir: tmp.path().join("stages"),
            allow_local_dev_tags: true,
        })
        .expect("runtime"),
    );
    runtime.register_catalog(catalog).await.expect("register catalog");

    let org_id = OrgId::new("org-nb").expect("org id");
    let grant = DatasetGrant::scoped_rows_partner(org_id.clone(), catalog_id);
    let session = session_store
        .create_session(&org_id, "Notebook Bridge Session", &grant.grant_id)
        .await
        .expect("session");

    let art_key = "artifacts/test/report.html";
    let sha = blob_store
        .put_bytes(art_key, b"<html><body>OK</body></html>")
        .await
        .expect("put artifact");

    let run_id = RunId::new("run-nb-bridge-1").expect("run id");
    let plugin_id = PluginId::new("alithea/bc-jupyter-notebook").expect("pid");
    let big_b64 = "A".repeat(4096);
    let now = Utc::now();
    let rec = RunRecord {
        run_id: run_id.clone(),
        org_id: org_id.clone(),
        session_id: Some(session.session_id.clone()),
        plugin_id: plugin_id.clone(),
        plugin_version: "1.0.0".into(),
        status: RunStatus::Succeeded,
        admission: None,
        params: json!({
            "notebook_title": "Test NB",
            "cells": [{ "cell_type": "code", "source": "print(1)" }]
        }),
        grant: Some(grant.clone()),
        inputs: Vec::new(),
        parent_run_id: None,
        agent_credential_id: None,
        agent_tool_call_id: None,
        module_fee_milli_act: None,
        dataset_fee_milli_act: None,
        quote: RunQuote {
            run_id: run_id.clone(),
            plugin_id,
            plugin_version: "1.0.0".into(),
            image_ref: "127.0.0.1:5500/alithea/bc-jupyter-notebook@sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
            target_node_id: NodeId::new("node-1").expect("node"),
            offer_id: "default".into(),
            offer_name: "Standard Run".into(),
            base_fee_act: CreditAmountAct(0),
            estimated_cost_act: CreditAmountAct(0),
            max_input_bytes: 10 * 1024 * 1024,
            max_output_bytes: 256 * 1024 * 1024,
            max_runtime_sec: 1800,
            max_memory_mb: 4096,
            workload_hash: "hash-1".into(),
            dataset_grant_summary: Some(grant.label.clone()),
            froglet_quote: json!({}),
            expires_at: now,
        },
        result_json: Some(json!({
            "notebook_title": "Test NB",
            "cells": [{
                "index": 0,
                "outputs": [{ "output_type": "display_data", "data": { "image/png": big_b64 } }]
            }]
        })),
        artifacts: vec![ArtifactRef {
            name: "notebook_report.html".into(),
            blob_key: art_key.into(),
            media_type: "text/html".into(),
            size_bytes: 28,
            sha256: sha,
        }],
        froglet_receipt: None,
        froglet_chain: vec![],
        receipt_verified: true,
        logs: vec![],
        error_message: None,
        created_at: now,
        completed_at: Some(now),
    };
    session_store.save_run(&rec).await.expect("save run");

    let listed = execute_agent_tool(
        &LlmToolCall {
            id: "c1".into(),
            name: "list_runs".into(),
            arguments: json!({ "plugin_id": "alithea/bc-jupyter-notebook", "limit": 5 }),
        },
        &org_id,
        &session.session_id,
        Some(&grant),
        runtime.as_ref(),
        session_store.as_ref(),
        blob_store.as_ref(),
        tmp.path(),
        None,
    )
    .await;
    assert_eq!(listed["runs"].as_array().expect("runs").len(), 1);
    assert_eq!(listed["runs"][0]["run_id"], json!(rec.run_id.as_str()));

    let res = execute_agent_tool(
        &LlmToolCall {
            id: "c2".into(),
            name: "get_run_result".into(),
            arguments: json!({ "run_id": rec.run_id.as_str() }),
        },
        &org_id,
        &session.session_id,
        Some(&grant),
        runtime.as_ref(),
        session_store.as_ref(),
        blob_store.as_ref(),
        tmp.path(),
        None,
    )
    .await;
    let omitted = res["result_json"]["cells"][0]["outputs"][0]["data"]["image/png"]
        .as_str()
        .expect("redacted png placeholder");
    assert!(omitted.contains("base64 omitted"), "got {omitted}");

    let art = execute_agent_tool(
        &LlmToolCall {
            id: "c3".into(),
            name: "get_run_artifact".into(),
            arguments: json!({ "run_id": rec.run_id.as_str(), "artifact_name": "notebook_report.html" }),
        },
        &org_id,
        &session.session_id,
        Some(&grant),
        runtime.as_ref(),
        session_store.as_ref(),
        blob_store.as_ref(),
        tmp.path(),
        None,
    )
    .await;
    assert_eq!(art["content"], json!("<html><body>OK</body></html>"));

    let prompt = build_system_prompt(
        Some(&grant),
        &[],
        &[],
        Some(&json!({
            "plugin_id": "alithea/bc-jupyter-notebook",
            "draft_params": { "notebook_title": "Live Draft" },
            "latest_run": { "run_id": rec.run_id.as_str(), "status": "succeeded" }
        })),
    );
    assert!(prompt.contains("Active Open Module Context (Live UI Draft, Parameters & Outputs)"));
    assert!(prompt.contains("Live Draft"));
    assert!(prompt.contains(rec.run_id.as_str()));

    let plugin = LoadedPlugin::load_from_dir(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("repo root")
            .join("plugins/hla-motif-enrichment"),
        false,
    )
    .expect("load plugin");
    runtime.register_plugin(plugin).await.expect("reg plugin");

    let (tx, mut rx) = mpsc::channel::<AgentStreamEvent>(8);
    let draft_res = execute_agent_tool(
        &LlmToolCall {
            id: "c4".into(),
            name: "update_module_draft".into(),
            arguments: json!({
                "plugin_id": "alithea/hla-motif-enrichment",
                "params": { "min_length": 9, "max_length": 11 },
                "open_tab": "run"
            }),
        },
        &org_id,
        &session.session_id,
        Some(&grant),
        runtime.as_ref(),
        session_store.as_ref(),
        blob_store.as_ref(),
        tmp.path(),
        Some(&tx),
    )
    .await;
    assert_eq!(draft_res["status"], json!("applied_to_open_module_ui"));
    let ev = rx.recv().await.expect("stream event");
    match ev {
        AgentStreamEvent::ModuleDraftUpdated {
            plugin_id,
            params,
            merge,
            open_tab,
        } => {
            assert_eq!(plugin_id, "alithea/hla-motif-enrichment");
            assert_eq!(params["min_length"], json!(9));
            assert!(merge);
            assert_eq!(open_tab.as_deref(), Some("run"));
        }
        other => panic!("unexpected stream event: {other:?}"),
    }
}
