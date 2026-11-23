#![allow(missing_docs)]

use biocompass_agent_core::llm::{LlmProviderConfig, LlmWireProtocol};
use biocompass_agent_core::mcp::ScopedMcpServer;
use biocompass_agent_core::{AgentConfig, AgentRuntime, AgentStreamEvent};
use biocompass_core_traits::{
    CreditAmountAct, DatasetGrant, LocalFsBlobStore, OrgId, SessionEventKind, SessionRecord,
    SessionStore, SqliteActLedger, SqliteSessionStore,
};
use biocompass_data_engine::{DuckDbCatalogEngine, seed_demo_hla_catalog};
use biocompass_node_runtime::{InProcessNodeRuntime, NodeKeypairSet, NodeRuntimeDeps};
use biocompass_oci_runner::{OciContainerRunner, OciDistributionClient};
use reqwest::Client;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;

async fn build_test_runtime(tmp: &TempDir) -> (Arc<SqliteSessionStore>, Arc<LocalFsBlobStore>, Arc<InProcessNodeRuntime>, DatasetGrant, SessionRecord) {
    let keys = NodeKeypairSet::load_or_generate(tmp.path().join("keys")).expect("keys");
    let session_store = Arc::new(SqliteSessionStore::in_memory().expect("session store"));
    let blob_store = Arc::new(LocalFsBlobStore::new(tmp.path().join("blobs")).expect("blob store"));
    let ledger = Arc::new(SqliteActLedger::in_memory(CreditAmountAct::from_whole_act(50)).expect("ledger"));
    let oci_registry = Arc::new(OciDistributionClient::new(true).expect("oci registry"));
    let oci_runner = Arc::new(OciContainerRunner::detect().await.expect("oci runner"));

    let runtime = Arc::new(
        InProcessNodeRuntime::new(NodeRuntimeDeps {
            keys,
            session_store: session_store.clone(),
            blob_store: blob_store.clone(),
            oci_registry,
            payment_gateway: ledger,
            oci_runner,
            stage_root_dir: tmp.path().join("stages"),
            allow_local_dev_tags: false,
        })
        .expect("node runtime"),
    );

    let catalog_dir = tmp.path().join("catalog");
    seed_demo_hla_catalog(&catalog_dir).expect("seed catalog");
    let catalog = DuckDbCatalogEngine::open_verified(&catalog_dir).expect("open catalog");
    let catalog_id = catalog.manifest().id.clone();
    runtime.register_catalog(catalog).await.expect("reg catalog");

    let org_id = OrgId::new("org-acp-test").expect("org id");
    let grant = DatasetGrant::scoped_rows_partner(org_id.clone(), catalog_id);
    let session = session_store
        .create_session(&org_id, "ACP & MCP Integration Test", &grant.grant_id)
        .await
        .expect("create session");

    (session_store, blob_store, runtime, grant, session)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scoped_mcp_server_enforces_token_and_consent_gate() {
    let tmp = TempDir::new().expect("tempdir");
    let (session_store, blob_store, runtime, grant, session) = build_test_runtime(&tmp).await;

    let mcp = ScopedMcpServer::start(
        session.org_id.clone(),
        session.session_id.clone(),
        Some(grant.clone()),
        runtime,
        session_store.clone(),
        blob_store,
        tmp.path().join("scaffold"),
        48_000,
        None,
    )
    .await
    .expect("start mcp");

    let client = Client::new();
    let bad_url = format!("{}-wrong", mcp.url());
    let bad_res = client
        .post(&bad_url)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
        .send()
        .await
        .expect("bad token request");
    assert_eq!(bad_res.status().as_u16(), 404);

    let init_res: Value = client
        .post(mcp.url())
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}))
        .send()
        .await
        .expect("initialize")
        .json()
        .await
        .expect("decode init");
    assert_eq!(init_res["result"]["protocolVersion"], "2025-03-26");

    let list_res: Value = client
        .post(mcp.url())
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}))
        .send()
        .await
        .expect("tools/list")
        .json()
        .await
        .expect("decode tools/list");
    let tools = list_res["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 8);
    assert!(!tools.iter().any(|t| t["name"] == "confirm_run"));

    let call_res: Value = client
        .post(mcp.url())
        .json(&json!({
            "jsonrpc":"2.0",
            "id":3,
            "method":"tools/call",
            "params":{"name":"list_catalogs","arguments":{}}
        }))
        .send()
        .await
        .expect("tools/call")
        .json()
        .await
        .expect("decode tools/call");
    assert_eq!(call_res["result"]["isError"], false);

    let denied_call: Value = client
        .post(mcp.url())
        .json(&json!({
            "jsonrpc":"2.0",
            "id":4,
            "method":"tools/call",
            "params":{"name":"confirm_run","arguments":{}}
        }))
        .send()
        .await
        .expect("confirm_run blocked")
        .json()
        .await
        .expect("decode blocked");
    assert_eq!(denied_call["result"]["isError"], true);

    let metrics = mcp.finish().await;
    assert_eq!(metrics.tool_calls_executed, 1);

    let events = session_store
        .list_events(&session.session_id)
        .await
        .expect("list events");
    assert!(events.iter().any(|e| e.kind == SessionEventKind::ToolCall && e.content == "list_catalogs"));
    assert!(events.iter().any(|e| e.kind == SessionEventKind::ToolResult));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_stdio_turn_connects_mcp_filters_permissions_and_streams() {
    let tmp = TempDir::new().expect("tempdir");
    let (session_store, blob_store, runtime, grant, session) = build_test_runtime(&tmp).await;

    let script_path: PathBuf = tmp.path().join("mock_acp.py");
    let script = r#"import json, sys, urllib.request

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

mcp_url = None
for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    msg = json.loads(raw)
    method = msg.get("method")
    req_id = msg.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": req_id, "result": {"protocolVersion": 1, "agentInfo": {"name": "mock-acp"}}})
    elif method == "session/new":
        servers = msg.get("params", {}).get("mcpServers", [])
        mcp_url = servers[0]["url"]
        send({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "sessionId": "sess_mock_1",
                "configOptions": [{"id": "model", "currentValue": "default", "options": [{"value": "mock-pro"}]}]
            }
        })
        # Simulate external MCP failure notice emitted asynchronously by user CLI profile during warmup
        send({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess_mock_1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {
                        "type": "text",
                        "text": "The following MCP servers failed to connect:\n- google_drive_mcp: Connector bootstrap issue\n- hla-compass: authentication was rejected"
                    }
                }
            }
        })
    elif method == "session/set_config_option":
        send({"jsonrpc": "2.0", "id": req_id, "result": {}})
    elif method == "session/prompt":
        send({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess_mock_1",
                "update": {"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "Planning catalog check."}}
            }
        })
        # 1. Verify bash permission is denied by harness
        send({
            "jsonrpc": "2.0",
            "id": 9001,
            "method": "session/request_permission",
            "params": {
                "toolCall": {"title": "bash", "kind": "execute"},
                "options": [{"optionId": "allow_once", "kind": "allow_once"}, {"optionId": "reject_once", "kind": "reject_once"}]
            }
        })
        bash_perm = json.loads(sys.stdin.readline())
        assert bash_perm["result"]["outcome"]["optionId"] == "reject_once", bash_perm

        # 2. Verify biocompass MCP tool permission is allowed by harness (including toolCallId-only like Vibe ACP)
        send({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess_mock_1",
                "update": {"sessionUpdate": "tool_call", "toolCallId": "tc_vibe_1", "title": "mcp_biocompass.list_catalogs", "kind": "other"}
            }
        })
        send({
            "jsonrpc": "2.0",
            "id": 9002,
            "method": "session/request_permission",
            "params": {
                "toolCall": {"toolCallId": "tc_vibe_1"},
                "options": [{"optionId": "allow_once", "kind": "allow_once"}, {"optionId": "reject_once", "kind": "reject_once"}]
            }
        })
        mcp_perm = json.loads(sys.stdin.readline())
        assert mcp_perm["result"]["outcome"]["optionId"] == "allow_once", mcp_perm

        # 3. Call the ephemeral loopback MCP endpoint directly
        req = urllib.request.Request(
            mcp_url,
            data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "list_catalogs", "arguments": {}}}).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req) as resp:
            tool_out = json.loads(resp.read().decode())
        assert tool_out["result"]["isError"] is False

        send({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess_mock_1",
                "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "ACP catalog check passed."}}
            }
        })
        send({"jsonrpc": "2.0", "id": req_id, "result": {"stopReason": "end_turn"}})
"#;
    std::fs::write(&script_path, script).expect("write mock acp script");

    let agent = AgentRuntime::new(
        AgentConfig {
            llm: Some(LlmProviderConfig {
                protocol: LlmWireProtocol::AcpStdio,
                base_url: String::new(),
                api_key: String::new(),
                model: "mock-pro".into(),
                max_tokens: 4096,
                command: "python3".into(),
                args: vec![script_path.to_string_lossy().into_owned()],
                config_options: Default::default(),
            }),
            max_tool_iterations: 4,
            spill_threshold_bytes: 48_000,
            scaffold_root_dir: tmp.path().join("scaffold"),
        },
        session_store.clone(),
        blob_store,
        runtime,
    )
    .expect("agent runtime");

    let (tx, mut rx) = mpsc::channel(32);
    let summary = agent
        .run_turn(&session.session_id, "Inspect catalogs via ACP", Some(&grant), Some(tx))
        .await
        .expect("run acp turn");

    assert_eq!(summary.assistant_text, "ACP catalog check passed.");
    assert_eq!(summary.tool_calls_executed, 1);

    let events = session_store
        .list_events(&session.session_id)
        .await
        .expect("list events");
    let assistant_ev = events
        .iter()
        .rfind(|e| matches!(e.kind, SessionEventKind::AssistantMessage))
        .expect("assistant event persisted");
    assert_eq!(
        assistant_ev.metadata.get("thinking").and_then(Value::as_str),
        Some("Planning catalog check.")
    );

    let mut saw_thought = false;
    let mut saw_delta = false;
    let mut saw_tool_call = false;
    let mut saw_completed = false;
    while let Ok(ev) = rx.try_recv() {
        match ev {
            AgentStreamEvent::ThoughtDelta(t) if t.contains("Planning catalog check.") => {
                saw_thought = true;
            }
            AgentStreamEvent::TextDelta(d) if d.contains("ACP catalog check passed.") => {
                saw_delta = true;
            }
            AgentStreamEvent::ToolCall { name, .. } if name == "list_catalogs" => {
                saw_tool_call = true;
            }
            AgentStreamEvent::Completed { .. } => {
                saw_completed = true;
            }
            _ => {}
        }
    }
    assert!(saw_thought && saw_delta && saw_tool_call && saw_completed);
}
