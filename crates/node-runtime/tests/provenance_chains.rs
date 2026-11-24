use async_trait::async_trait;
use biocompass_core_traits::{
    ErrorCode, ExtensionRefStage, LocalFsBlobStore, OciImageRef,
    OciRegistryClient, OrgId, PluginId, PrepareRunRequest, RunDispatcher, RunId, RunInput,
    RunStatus, SqliteSessionStore, expected_extension_refs, verify_extension_refs,
};
use biocompass_node_runtime::{
    InProcessNodeRuntime, NodeKeypairSet, NodeRuntimeDeps, verify_froglet_chain_offline,
    verify_released_result_commitment,
};
use biocompass_oci_runner::{
    ChildModuleRequest, ChildRunBroker, ChildRunBrokerContext, ChildRunReply,
    ChildRunStagedArtifact, DetectedEngine, EngineKind, OciContainerRunner, child_artifact_alias,
    stage_child_artifact_bytes,
};
use biocompass_plugin_spec::LoadedPlugin;
use froglet_protocol::crypto::sha256_hex;
use froglet_protocol::protocol::{DealPayload, QuotePayload, ReceiptPayload, SignedArtifact};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

struct LocalTagRegistry;

#[async_trait]
impl OciRegistryClient for LocalTagRegistry {
    async fn verify_image(&self, image_raw: &str) -> biocompass_core_traits::CompassResult<OciImageRef> {
        OciImageRef::parse(image_raw, true)
    }
}

fn write_plugin_manifest(dir: &Path, id: &str, max_input_bytes: u64, child_modules: &[&str]) -> LoadedPlugin {
    std::fs::create_dir_all(dir).unwrap();
    let children_toml = if child_modules.is_empty() {
        String::new()
    } else if child_modules == ["caller"] {
        "child_modules = \"caller\"\nmax_child_wait_sec = 60\nmax_child_spend_milli_eur = 0\n".to_owned()
    } else {
        let quoted: Vec<String> = child_modules.iter().map(|m| format!("\"{m}\"")).collect();
        format!(
            "child_modules = [{}]\nmax_child_wait_sec = 60\n",
            quoted.join(", ")
        )
    };
    let toml = format!(
        r#"
id = "{id}"
name = "Test Module"
version = "1.0.0"
image = "local/{slug}:dev"
network = "none"
{children_toml}
[resources]
cpus = 1
memory_mb = 256
timeout_sec = 10

[[offers]]
id = "default"
name = "Default"
price_milli_eur = 0
max_input_bytes = {max_input_bytes}
max_output_bytes = 1048576

[inputs.schema]
type = "object"
"#,
        slug = id.split_once('/').unwrap().1,
    );
    std::fs::write(dir.join("plugin.toml"), toml).unwrap();
    std::fs::write(dir.join("SKILL.md"), "Test module").unwrap();
    LoadedPlugin::load_from_dir(dir, true).unwrap()
}

fn fake_docker_script() -> &'static str {
    r#"#!/bin/sh
set -eu
case "$1" in
  run)
    in_host=""
    out_host=""
    prev=""
    for arg in "$@"; do
      if [ "$prev" = "-v" ]; then
        case "$arg" in
          *:/in:ro) in_host="${arg%:/in:ro}" ;;
          *:/out:rw) out_host="${arg%:/out:rw}" ;;
        esac
        prev=""
      elif [ "$arg" = "-v" ]; then
        prev="-v"
      fi
    done
    params="$(cat "$in_host/params.json")"
    mkdir -p "$out_host/artifacts"
    case "$params" in
      *'"mode":"producer"'*)
        printf 'allele,score\nA0201,0.95\n' > "$out_host/artifacts/scores.csv"
        printf '{"role":"producer","rows":1}\n' > "$out_host/result.json"
        ;;
      *'"mode":"consumer"'*)
        test -f "$in_host/artifacts/upstream/scores.csv"
        content="$(cat "$in_host/artifacts/upstream/scores.csv")"
        printf 'consumed:%s' "$content" > "$out_host/artifacts/summary.txt"
        printf '{"role":"consumer","verified_input":true}\n' > "$out_host/result.json"
        ;;
      *'"mode":"parent"'*)
        req_dir="$out_host/.bc/requests"
        rep_dir="$in_host/.bc/replies"
        printf '{"kind":"run_module","request_id":"call-1","plugin_id":"alithea/child","params":{"mode":"producer"}}\n' > "$req_dir/.tmp-call-1.tmp"
        mv "$req_dir/.tmp-call-1.tmp" "$req_dir/call-1.json"
        i=0
        while [ ! -f "$rep_dir/call-1.json" ] && [ "$i" -lt 100 ]; do
          sleep 0.05
          i=$((i + 1))
        done
        test -f "$rep_dir/call-1.json"
        test -f "$in_host/artifacts/child-call-1/scores.csv"
        printf '{"role":"parent","child_ok":true}\n' > "$out_host/result.json"
        ;;
      *)
        printf '{"ok":true}\n' > "$out_host/result.json"
        ;;
    esac
    exit 0
    ;;
  rm) exit 0 ;;
  *) exit 0 ;;
esac
"#
}

#[cfg(unix)]
fn build_runtime(tmp: &Path) -> (Arc<InProcessNodeRuntime>, Arc<LocalFsBlobStore>, Arc<SqliteSessionStore>) {
    use std::os::unix::fs::PermissionsExt;
    let fake_docker = tmp.join("fake-docker");
    std::fs::write(&fake_docker, fake_docker_script()).unwrap();
    std::fs::set_permissions(&fake_docker, std::fs::Permissions::from_mode(0o700)).unwrap();

    let keys = NodeKeypairSet::load_or_generate(tmp.join("keys")).unwrap();
    let session_store = Arc::new(SqliteSessionStore::in_memory().unwrap());
    let blob_store = Arc::new(LocalFsBlobStore::new(tmp.join("blobs")).unwrap());
    let oci_runner = Arc::new(OciContainerRunner::from_engine(DetectedEngine {
        kind: EngineKind::Docker,
        binary_path: fake_docker,
        server_version: "test".into(),
        rootless: false,
    }));
    let runtime = Arc::new(
        InProcessNodeRuntime::new(NodeRuntimeDeps {
            keys,
            session_store: session_store.clone(),
            blob_store: blob_store.clone(),
            oci_registry: Arc::new(LocalTagRegistry),
            oci_runner,
            stage_root_dir: tmp.join("stages"),
            allow_local_dev_tags: true,
        })
        .unwrap(),
    );
    (runtime, blob_store, session_store)
}

#[cfg(unix)]
#[tokio::test]
async fn w6_stages_verified_run_inputs_enforces_max_input_bytes_and_binds_extension_refs() {
    let tmp = tempfile::tempdir().unwrap();
    let (runtime, blob_store, _) = build_runtime(tmp.path());

    let producer = write_plugin_manifest(&tmp.path().join("p-prod"), "alithea/producer", 4096, &[]);
    let consumer = write_plugin_manifest(&tmp.path().join("p-cons"), "alithea/consumer", 4096, &[]);
    let tight = write_plugin_manifest(&tmp.path().join("p-tight"), "alithea/tight", 64, &[]);
    runtime.register_plugin(producer).await.unwrap();
    runtime.register_plugin(consumer).await.unwrap();
    runtime.register_plugin(tight).await.unwrap();

    let org_id = OrgId::new("org-w6").unwrap();

    // 1. Run A (producer) writes /out/artifacts/scores.csv
    let quote_a = runtime
        .prepare_run(PrepareRunRequest {
            org_id: org_id.clone(),
            session_id: None,
            plugin_id: PluginId::from("alithea/producer"),
            plugin_version: None,
            offer_id: None,
            params: json!({ "mode": "producer" }),
            dataset_grant: None,
            inputs: Vec::new(),
            parent_run_id: None,
            agent_credential_id: None,
            agent_tool_call_id: None,
            doc_id: None,
            doc_version: None,
        })
        .await
        .unwrap();
    let run_a = runtime.confirm_run(&quote_a.run_id, "idem-a").await.unwrap();
    assert_eq!(run_a.status, RunStatus::Succeeded);
    assert!(run_a.receipt_verified);
    assert_eq!(run_a.artifacts.len(), 1);
    let art_a = &run_a.artifacts[0];
    let receipt_a: SignedArtifact<ReceiptPayload> =
        serde_json::from_value(run_a.froglet_chain[4].clone()).unwrap();

    let input_from_a = RunInput {
        alias: "upstream".into(),
        source_run_id: run_a.run_id.to_string(),
        name: art_a.name.clone(),
        sha256: art_a.sha256.clone(),
        size_bytes: art_a.size_bytes,
        source_receipt_hash: receipt_a.hash.clone(),
    };

    // 2. Overflowing Offer max_input_bytes (params + RunInput > 64 bytes) MUST fail closed with InputLimitExceeded!
    let overflow_err = runtime
        .prepare_run(PrepareRunRequest {
            org_id: org_id.clone(),
            session_id: None,
            plugin_id: PluginId::from("alithea/tight"),
            plugin_version: None,
            offer_id: None,
            params: json!({ "mode": "consumer", "pad": "012345678901234567890123456789" }),
            dataset_grant: None,
            inputs: vec![input_from_a.clone()],
            parent_run_id: None,
            agent_credential_id: None,
            agent_tool_call_id: None,
            doc_id: None,
            doc_version: None,
        })
        .await
        .expect_err("params + RunInput exceeding max_input_bytes must be rejected");
    assert_eq!(overflow_err.error_code(), ErrorCode::InputLimitExceeded);

    // 3. Run B (consumer) consumes Run A's artifact at /in/artifacts/upstream/scores.csv
    let quote_b = runtime
        .prepare_run(PrepareRunRequest {
            org_id: org_id.clone(),
            session_id: None,
            plugin_id: PluginId::from("alithea/consumer"),
            plugin_version: None,
            offer_id: None,
            params: json!({ "mode": "consumer" }),
            dataset_grant: None,
            inputs: vec![input_from_a.clone()],
            parent_run_id: None,
            agent_credential_id: None,
            agent_tool_call_id: None,
            doc_id: None,
            doc_version: None,
        })
        .await
        .unwrap();
    let run_b = runtime.confirm_run(&quote_b.run_id, "idem-b").await.unwrap();
    assert_eq!(run_b.status, RunStatus::Succeeded, "err={:?}", run_b.error_message);
    assert!(run_b.receipt_verified);
    assert!(verify_froglet_chain_offline(&run_b.froglet_chain, None).valid);

    let signed_quote_b: SignedArtifact<QuotePayload> =
        serde_json::from_value(run_b.froglet_chain[2].clone()).unwrap();
    let signed_deal_b: SignedArtifact<DealPayload> =
        serde_json::from_value(run_b.froglet_chain[3].clone()).unwrap();
    let expected_refs = expected_extension_refs(
        ExtensionRefStage::QuoteOrDeal,
        std::slice::from_ref(&input_from_a),
        None,
        &[],
    )
    .unwrap();
    assert_eq!(signed_quote_b.payload.extension_refs, expected_refs);
    assert_eq!(signed_deal_b.payload.extension_refs, expected_refs);
    verify_extension_refs(
        ExtensionRefStage::QuoteOrDeal,
        &signed_deal_b.payload.extension_refs,
        std::slice::from_ref(&input_from_a),
        None,
        &[],
    )
    .unwrap();

    // 4. Tampering the stored upstream blob bytes MUST fail Run C during staging (`verify_released_artifact`)!
    let quote_c = runtime
        .prepare_run(PrepareRunRequest {
            org_id: org_id.clone(),
            session_id: None,
            plugin_id: PluginId::from("alithea/consumer"),
            plugin_version: None,
            offer_id: None,
            params: json!({ "mode": "consumer" }),
            dataset_grant: None,
            inputs: vec![input_from_a],
            parent_run_id: None,
            agent_credential_id: None,
            agent_tool_call_id: None,
            doc_id: None,
            doc_version: None,
        })
        .await
        .unwrap();
    use biocompass_core_traits::BlobStore;
    blob_store
        .put_bytes(&art_a.blob_key, b"TAMPERED BYTES!")
        .await
        .unwrap();
    let run_c = runtime.confirm_run(&quote_c.run_id, "idem-c").await.unwrap();
    assert_eq!(run_c.status, RunStatus::Failed);
    assert!(
        run_c
            .error_message
            .as_deref()
            .unwrap_or("")
            .contains("failed its length or digest check"),
        "expected verify_released_artifact failure, got {:?}",
        run_c.error_message
    );
}

#[cfg(unix)]
#[tokio::test]
async fn w6_w7_parent_and_child_runs_bind_parent_deal_and_child_receipt_extension_refs() {
    struct LocalChildBroker {
        runtime: Arc<InProcessNodeRuntime>,
        blob_store: Arc<LocalFsBlobStore>,
        org_id: OrgId,
    }

    #[async_trait]
    impl ChildRunBroker for LocalChildBroker {
        async fn run_child(
            &self,
            ctx: &ChildRunBrokerContext,
            request: ChildModuleRequest,
        ) -> biocompass_core_traits::CompassResult<ChildRunReply> {
            let child_quote = self
                .runtime
                .prepare_run(PrepareRunRequest {
                    org_id: self.org_id.clone(),
                    session_id: None,
                    plugin_id: request.plugin_id,
                    plugin_version: request.plugin_version,
                    offer_id: request.offer_id,
                    params: request.params,
                    dataset_grant: None,
                    inputs: request.inputs,
                    parent_run_id: Some(ctx.parent_run_id.to_string()),
                    agent_credential_id: None,
                    agent_tool_call_id: None,
                    doc_id: None,
                    doc_version: None,
                })
                .await?;
            let child_run = self
                .runtime
                .confirm_run(&child_quote.run_id, &format!("child-{}", request.request_id))
                .await?;
            let receipt_hash = child_run
                .froglet_receipt
                .as_ref()
                .and_then(|r| r.get("hash"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let alias = child_artifact_alias(&request.request_id);
            let mut staged = Vec::new();
            use biocompass_core_traits::BlobStore;
            for art in &child_run.artifacts {
                let bytes = self.blob_store.get_bytes(&art.blob_key).await?;
                let path = stage_child_artifact_bytes(&ctx.parent_in_dir, &alias, &art.name, &bytes)?;
                staged.push(ChildRunStagedArtifact {
                    name: art.name.clone(),
                    sha256: art.sha256.clone(),
                    size_bytes: art.size_bytes,
                    media_type: art.media_type.clone(),
                    staged_path: Some(path),
                });
            }
            Ok(ChildRunReply {
                request_id: request.request_id,
                ok: child_run.status == RunStatus::Succeeded,
                status: Some(child_run.status),
                run_id: Some(child_run.run_id.to_string()),
                receipt_hash,
                result_json: child_run.result_json,
                result_path: None,
                artifacts: staged,
                error_code: None,
                error: child_run.error_message,
            })
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let (runtime, blob_store, session_store) = build_runtime(tmp.path());

    let parent_mod =
        write_plugin_manifest(&tmp.path().join("p-parent"), "alithea/parent", 4096, &["alithea/child"]);
    let child_mod = write_plugin_manifest(&tmp.path().join("p-child"), "alithea/child", 4096, &[]);
    runtime.register_plugin(parent_mod).await.unwrap();
    runtime.register_plugin(child_mod).await.unwrap();

    let org_id = OrgId::new("org-w7").unwrap();
    let parent_quote = runtime
        .prepare_run(PrepareRunRequest {
            org_id: org_id.clone(),
            session_id: None,
            plugin_id: PluginId::from("alithea/parent"),
            plugin_version: None,
            offer_id: None,
            params: json!({ "mode": "parent" }),
            dataset_grant: None,
            inputs: Vec::new(),
            parent_run_id: None,
            agent_credential_id: None,
            agent_tool_call_id: None,
            doc_id: None,
            doc_version: None,
        })
        .await
        .unwrap();

    let broker = Arc::new(LocalChildBroker {
        runtime: runtime.clone(),
        blob_store: blob_store.clone(),
        org_id: org_id.clone(),
    });
    let parent_run = runtime
        .confirm_run_with_mailbox(&parent_quote.run_id, "idem-parent", None, None, Some(broker))
        .await
        .unwrap();
    assert_eq!(parent_run.status, RunStatus::Succeeded, "err={:?}", parent_run.error_message);
    assert!(verify_froglet_chain_offline(&parent_run.froglet_chain, None).valid);

    let parent_deal: SignedArtifact<DealPayload> =
        serde_json::from_value(parent_run.froglet_chain[3].clone()).unwrap();
    let parent_receipt: SignedArtifact<ReceiptPayload> =
        serde_json::from_value(parent_run.froglet_chain[4].clone()).unwrap();
    assert_eq!(parent_receipt.payload.extension_refs.len(), 1);
    let child_receipt_ref = &parent_receipt.payload.extension_refs[0];
    let child_receipt_hash = child_receipt_ref
        .strip_prefix("bc.child.v1:")
        .expect("parent receipt must carry bc.child.v1:<hash>");

    // Locate the child run in SessionStore and verify its Quote & Deal carry bc.parent.v1:<parent_deal_hash>!
    use biocompass_core_traits::SessionStore;
    let all_runs = session_store.list_runs(&org_id).await.unwrap();
    let child_run = all_runs
        .iter()
        .find(|r| r.parent_run_id.as_deref() == Some(parent_run.run_id.as_str()))
        .expect("child run persisted with parent_run_id");
    let child_quote: SignedArtifact<QuotePayload> =
        serde_json::from_value(child_run.froglet_chain[2].clone()).unwrap();
    let child_deal: SignedArtifact<DealPayload> =
        serde_json::from_value(child_run.froglet_chain[3].clone()).unwrap();
    let child_receipt: SignedArtifact<ReceiptPayload> =
        serde_json::from_value(child_run.froglet_chain[4].clone()).unwrap();
    assert_eq!(child_receipt.hash, child_receipt_hash);
    assert_eq!(
        child_quote.payload.extension_refs,
        vec![format!("bc.parent.v1:{}", parent_deal.hash)]
    );
    assert_eq!(
        child_deal.payload.extension_refs,
        vec![format!("bc.parent.v1:{}", parent_deal.hash)]
    );

    // Depth-2 grandchild MUST be rejected!
    let grandchild_err = runtime
        .prepare_run(PrepareRunRequest {
            org_id: org_id.clone(),
            session_id: None,
            plugin_id: PluginId::from("alithea/child"),
            plugin_version: None,
            offer_id: None,
            params: json!({ "mode": "producer" }),
            dataset_grant: None,
            inputs: Vec::new(),
            parent_run_id: Some(child_run.run_id.to_string()),
            agent_credential_id: None,
            agent_tool_call_id: None,
            doc_id: None,
            doc_version: None,
        })
        .await
        .expect_err("depth-2 grandchild must be rejected");
    assert!(grandchild_err.to_string().contains("depth 1"), "{grandchild_err}");

    // Also test released_result::verify_released_result_commitment rejects mismatched RunInput metadata!
    let mut released_fixture: biocompass_core_traits::RunRecord = {
        let mut value: Value =
            serde_json::from_str(include_str!("fixtures/released_run.json")).unwrap();
        value["quote"]["froglet_quote"] = value["froglet_chain"][2].clone();
        value["froglet_receipt"] = value["froglet_chain"][4].clone();
        serde_json::from_value(value).unwrap()
    };
    let catalog = biocompass_core_traits::CatalogId::from("fixture/table");
    let digest = "22".repeat(32);
    let source = Some((&catalog, 2, digest.as_str()));
    let result_sha = sha256_hex(b" null\n");
    verify_released_result_commitment(&released_fixture, &result_sha, source).unwrap();

    // Injecting an unclaimed parent_run_id on a run whose signed Quote has no bc.parent.v1 MUST fail!
    released_fixture.parent_run_id = Some(RunId::from("run-forged-parent").to_string());
    assert!(
        verify_released_result_commitment(&released_fixture, &result_sha, source).is_err(),
        "forged parent_run_id without bc.parent.v1 must fail closed"
    );
}


/// N-1: on the laptop a parent declaring `child_modules = "caller"` confirmed through the plain
/// `RunDispatcher::confirm_run` path gets the in-process child broker: the child runs locally,
/// is linked with `bc.parent.v1`, and the parent receipt carries `bc.child.v1`.
#[cfg(unix)]
#[tokio::test]
async fn n1_laptop_runtime_brokers_caller_child_runs_through_plain_confirm() {
    let tmp = tempfile::tempdir().unwrap();
    let (runtime, _blob_store, session_store) = build_runtime(tmp.path());
    runtime.enable_local_child_runs();

    let parent_mod =
        write_plugin_manifest(&tmp.path().join("p-parent"), "alithea/parent", 4096, &["caller"]);
    let child_mod = write_plugin_manifest(&tmp.path().join("p-child"), "alithea/child", 4096, &[]);
    runtime.register_plugin(parent_mod).await.unwrap();
    runtime.register_plugin(child_mod).await.unwrap();

    let org_id = OrgId::new("org-n1").unwrap();
    let parent_quote = runtime
        .prepare_run(PrepareRunRequest {
            org_id: org_id.clone(),
            session_id: None,
            plugin_id: PluginId::from("alithea/parent"),
            plugin_version: None,
            offer_id: None,
            params: json!({ "mode": "parent" }),
            dataset_grant: None,
            inputs: Vec::new(),
            parent_run_id: None,
            agent_credential_id: None,
            agent_tool_call_id: None,
            doc_id: None,
            doc_version: None,
        })
        .await
        .unwrap();
    let parent_run = runtime.confirm_run(&parent_quote.run_id, "idem-n1-parent").await.unwrap();
    assert_eq!(parent_run.status, RunStatus::Succeeded, "err={:?}", parent_run.error_message);

    use biocompass_core_traits::SessionStore;
    let child_run = session_store
        .list_runs(&org_id)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.parent_run_id.as_deref() == Some(parent_run.run_id.as_str()))
        .expect("laptop broker persisted the child run");
    assert_eq!(child_run.status, RunStatus::Succeeded);
    let parent_receipt: SignedArtifact<ReceiptPayload> =
        serde_json::from_value(parent_run.froglet_chain[4].clone()).unwrap();
    let child_receipt_hash = child_run.froglet_receipt.as_ref().unwrap()["hash"].as_str().unwrap();
    assert_eq!(
        parent_receipt.payload.extension_refs,
        vec![format!("bc.child.v1:{child_receipt_hash}")]
    );
}
