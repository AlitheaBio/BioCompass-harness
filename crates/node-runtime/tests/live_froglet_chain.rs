//! Gate 3 live integration & adversarial cryptographic suite for `biocompass-node-runtime`.
//!
//! Verifies:
//! 1. Full `Prepare -> Confirm` lifecycle executing real `127.0.0.1:5500/alithea/hla-motif-enrichment@sha256:...`
//!    over pre-masked DuckDB Parquet cohorts with double-entry ACT `reserve -> settle`.
//! 2. External standalone `froglet-verify` CLI (`--chain --json`) returning exit code `0` and `"valid": true`
//!    on the exported 5-artifact `Descriptor -> Offer -> Quote -> Deal -> Receipt` chain.
//! 3. Four adversarial 1-byte mutations (`result_hash`, `workload_hash`, `deal_hash`, `signature`)
//!    all rejected with exit code `1` (`"valid": false`) by the external `froglet-verify` CLI.
//! 4. Forced non-zero container exit (`exit 2` on missing cohort) automatically refunds 100% of
//!    reserved ACT credits (`LedgerTxKind::RunRefund`) AND produces a valid `failed` Froglet chain.

use biocompass_core_traits::{
    BlobStore, CreditAmountAct, DatasetGrant, LedgerTxKind, LocalFsBlobStore, OrgId,
    PaymentGateway, PrepareRunRequest, RunDispatcher, RunStatus, SessionStore, SqliteActLedger,
    SqliteSessionStore,
};
use biocompass_data_engine::{DuckDbCatalogEngine, seed_demo_hla_catalog};
use biocompass_node_runtime::{InProcessNodeRuntime, NodeKeypairSet, NodeRuntimeDeps};
use biocompass_oci_runner::{OciContainerRunner, OciDistributionClient};
use biocompass_plugin_spec::LoadedPlugin;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tempfile::TempDir;

fn froglet_verify_binary() -> PathBuf {
    let bin = PathBuf::from("/tmp/froglet-clean-cli/target/debug/froglet-verify");
    if bin.is_file() {
        return bin;
    }

    // Build cleanly in /tmp/froglet-clean-cli from immutable git commit without touching armanas/froglet.
    let status = Command::new("sh")
        .args([
            "-c",
            "rm -rf /tmp/froglet-clean-cli && mkdir -p /tmp/froglet-clean-cli && \
             git -C /Users/armanas/Projects/github.com/armanas/froglet archive c6c5dc53f38a2c485f95cd6776f32f4e7b33785b | tar -x -C /tmp/froglet-clean-cli && \
             cargo build --manifest-path /tmp/froglet-clean-cli/froglet-verify/Cargo.toml --bin froglet-verify",
        ])
        .status()
        .expect("build standalone froglet-verify CLI");
    assert!(status.success(), "building standalone froglet-verify CLI failed");
    assert!(bin.is_file(), "froglet-verify binary missing at {}", bin.display());
    bin
}

fn flip_first_hex_nibble(hex_str: &str) -> String {
    assert!(!hex_str.is_empty(), "hex string must not be empty");
    let mut chars: Vec<char> = hex_str.chars().collect();
    chars[0] = if chars[0] == '0' { '1' } else { '0' };
    chars.into_iter().collect()
}

fn run_external_froglet_verify(bin: &Path, chain_docs: &[Value], tmp: &Path, name: &str) -> (i32, Value) {
    let path = tmp.join(name);
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(chain_docs).expect("serialize chain docs"),
    )
    .expect("write chain file");

    let output = Command::new(bin)
        .args(["--chain", "--json", path.to_str().expect("utf8 path")])
        .output()
        .expect("invoke standalone froglet-verify binary");

    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let report: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("froglet-verify did not emit JSON (code={code}, err={e}): stdout={stdout}, stderr={stderr}")
    });
    (code, report)
}

#[tokio::test]
async fn gate3_live_froglet_chain_external_verifier_tamper_and_refund_suite() {
    let verify_bin = froglet_verify_binary();
    let workspace = TempDir::new().expect("create temp workspace");

    let keys = NodeKeypairSet::load_or_generate(workspace.path().join("keys"))
        .expect("generate secp256k1 keypair set");
    assert_ne!(
        keys.provider_id(),
        keys.requester_id(),
        "provider and requester keys must be distinct BIP340 identities"
    );

    // Verify persistent key reload determinism.
    let reloaded_keys = NodeKeypairSet::load_or_generate(workspace.path().join("keys"))
        .expect("reload secp256k1 keypair set");
    assert_eq!(keys.provider_id(), reloaded_keys.provider_id());
    assert_eq!(keys.requester_id(), reloaded_keys.requester_id());

    let session_store = Arc::new(
        SqliteSessionStore::open(workspace.path().join("sessions.sqlite"))
            .expect("open SqliteSessionStore"),
    );
    let blob_store = Arc::new(
        LocalFsBlobStore::new(workspace.path().join("blobs")).expect("open LocalFsBlobStore"),
    );
    let initial_balance = CreditAmountAct::from_whole_act(50);
    let ledger = Arc::new(
        SqliteActLedger::open(workspace.path().join("ledger.sqlite"), initial_balance)
            .expect("open SqliteActLedger"),
    );
    let oci_registry =
        Arc::new(OciDistributionClient::new(true).expect("create OciDistributionClient"));
    let oci_runner = Arc::new(
        OciContainerRunner::detect()
            .await
            .expect("detect active Docker/Podman container engine"),
    );

    let runtime = InProcessNodeRuntime::new(NodeRuntimeDeps {
        keys: keys.clone(),
        session_store: session_store.clone(),
        blob_store: blob_store.clone(),
        oci_registry,
        payment_gateway: ledger.clone(),
        oci_runner,
        stage_root_dir: workspace.path().join("stages"),
        allow_local_dev_tags: false,
    })
    .expect("initialize InProcessNodeRuntime");

    // Seed real DuckDB Parquet catalog and register real OCI plugin.
    let catalog_dir = workspace.path().join("catalog");
    seed_demo_hla_catalog(&catalog_dir).expect("seed demo HLA Parquet catalog");
    let catalog = DuckDbCatalogEngine::open_verified(&catalog_dir).expect("open verified catalog");
    let catalog_id = catalog.manifest().id.clone();
    runtime
        .register_catalog(catalog)
        .await
        .expect("register catalog");

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf();
    let plugin = LoadedPlugin::load_from_dir(
        repo_root.join("plugins/hla-motif-enrichment"),
        false,
    )
    .expect("load real pinned hla-motif-enrichment plugin");
    let plugin_id = plugin.manifest.id.clone();
    runtime
        .register_plugin(plugin)
        .await
        .expect("register plugin");

    let org_id = OrgId::new("org-alithea-partner").expect("org id");
    let session = session_store
        .create_session(&org_id, "Gate 3 Cryptographic Audit", "grant-partner")
        .await
        .expect("create session");

    // -------------------------------------------------------------------------
    // 1. Positive Full-Stack Run (`Prepare -> Confirm`) with Masked Partner Grant
    // -------------------------------------------------------------------------
    let partner_grant = DatasetGrant::restricted_partner_demo(org_id.clone(), catalog_id.clone());
    let quote = runtime
        .prepare_run(PrepareRunRequest {
            org_id: org_id.clone(),
            session_id: Some(session.session_id.clone()),
            plugin_id: plugin_id.clone(),
            params: json!({
                "alleles": ["HLA-A*02:01", "HLA-B*07:02"],
                "min_length": 8,
                "max_length": 11,
                "strong_binder_cutoff_nm": 50.0
            }),
            dataset_grant: Some(partner_grant),
        })
        .await
        .expect("prepare_run should succeed");

    // Prepare MUST NOT reserve or deduct ACT before user confirmation!
    let pre_confirm_balance = ledger
        .available_balance(&org_id)
        .await
        .expect("query balance");
    assert_eq!(pre_confirm_balance, initial_balance);

    let mut log_rx = runtime.subscribe_logs();
    let run = runtime
        .confirm_run(&quote.run_id, "idem-gate3-positive-001")
        .await
        .expect("confirm_run should succeed");

    assert_eq!(run.status, RunStatus::Succeeded, "run failed: {:?}", run.error_message);
    assert!(run.receipt_verified);
    assert_eq!(run.froglet_chain.len(), 5);
    assert_eq!(run.artifacts.len(), 1);
    assert_eq!(run.artifacts[0].name, "allele_binding_summary.csv");

    // Verify live log events streamed during run.
    let mut streamed_lines = Vec::new();
    while let Ok(ev) = log_rx.try_recv() {
        if ev.run_id == quote.run_id {
            streamed_lines.push(format!("[{}] {}", ev.stream, ev.line));
        }
    }
    assert!(
        streamed_lines.iter().any(|l| l.contains("Completed:")),
        "expected container stdout log over broadcast channel, got {streamed_lines:?}"
    );

    // Verify result_json reflects masked partner grant & Lung-only scope.
    let res = run.result_json.as_ref().expect("result.json present");
    assert_eq!(res["sequence_policy"], json!("masked_grant"));
    let top = res["top_candidates"].as_array().expect("top_candidates array");
    assert!(!top.is_empty());
    for row in top {
        assert_eq!(row["sequence"], json!("***"), "leaked unmasked sequence in result: {row}");
        assert_eq!(row["organ"], json!("Lung"), "leaked non-Lung organ in result: {row}");
    }

    // Verify BlobStore persisted result.json, CSV artifact, and froglet_chain.json.
    assert!(
        blob_store
            .exists(&format!("runs/{}/result.json", run.run_id))
            .await
            .expect("exists result.json")
    );
    assert!(
        blob_store
            .exists(&run.artifacts[0].blob_key)
            .await
            .expect("exists csv")
    );
    assert!(
        blob_store
            .exists(&format!("runs/{}/froglet_chain.json", run.run_id))
            .await
            .expect("exists froglet_chain.json")
    );

    // Verify ACT ledger settled cleanly (50.00 ACT - 5.00 ACT = 45.00 ACT).
    let post_settle_balance = ledger
        .available_balance(&org_id)
        .await
        .expect("post-settle balance");
    assert_eq!(post_settle_balance, CreditAmountAct::from_whole_act(45));

    // Verify ephemeral stage directory was cleaned up (0 leaked stage-* directories).
    let stage_entries: Vec<_> = std::fs::read_dir(workspace.path().join("stages"))
        .expect("read stages dir")
        .collect();
    assert!(
        stage_entries.is_empty(),
        "ephemeral stage directories leaked: {stage_entries:?}"
    );

    // -------------------------------------------------------------------------
    // 2. Standalone External `froglet-verify --chain --json` CLI Proof
    // -------------------------------------------------------------------------
    let (exit_code, report) = run_external_froglet_verify(
        &verify_bin,
        &run.froglet_chain,
        workspace.path(),
        "valid_chain.json",
    );
    assert_eq!(
        exit_code, 0,
        "external froglet-verify rejected chain: {report}"
    );
    assert_eq!(report["valid"], json!(true));
    assert_eq!(report["chain_evaluated"], json!(true));
    let verified_artifacts = report["artifacts"].as_array().expect("artifacts array");
    assert_eq!(verified_artifacts.len(), 5);
    for art in verified_artifacts {
        assert_eq!(art["status"], json!("verified"), "artifact not verified: {art}");
        assert_eq!(art["envelope_valid"], json!(true));
    }

    // Verify signer separation: provider signs Descriptor/Offer/Quote/Receipt; requester signs Deal.
    assert_eq!(run.froglet_chain[0]["signer"], json!(keys.provider_id()));
    assert_eq!(run.froglet_chain[1]["signer"], json!(keys.provider_id()));
    assert_eq!(run.froglet_chain[2]["signer"], json!(keys.provider_id()));
    assert_eq!(run.froglet_chain[3]["signer"], json!(keys.requester_id()));
    assert_eq!(run.froglet_chain[4]["signer"], json!(keys.provider_id()));

    // -------------------------------------------------------------------------
    // 3. Adversarial Tamper Suite (4 Distinct Mutations -> External CLI Exit 1)
    // -------------------------------------------------------------------------
    // 3a. Tamper receipt.payload.result_hash
    let mut tampered_result_hash = run.froglet_chain.clone();
    let orig_rh = tampered_result_hash[4]["payload"]["result_hash"]
        .as_str()
        .expect("result_hash string")
        .to_owned();
    tampered_result_hash[4]["payload"]["result_hash"] = json!(flip_first_hex_nibble(&orig_rh));
    let (code_rh, rep_rh) = run_external_froglet_verify(
        &verify_bin,
        &tampered_result_hash,
        workspace.path(),
        "tampered_result_hash.json",
    );
    assert_eq!(code_rh, 1);
    assert_eq!(rep_rh["valid"], json!(false));

    // 3b. Tamper quote.payload.workload_hash
    let mut tampered_workload = run.froglet_chain.clone();
    let orig_wh = tampered_workload[2]["payload"]["workload_hash"]
        .as_str()
        .expect("workload_hash string")
        .to_owned();
    tampered_workload[2]["payload"]["workload_hash"] = json!(flip_first_hex_nibble(&orig_wh));
    let (code_wh, rep_wh) = run_external_froglet_verify(
        &verify_bin,
        &tampered_workload,
        workspace.path(),
        "tampered_workload_hash.json",
    );
    assert_eq!(code_wh, 1);
    assert_eq!(rep_wh["valid"], json!(false));

    // 3c. Tamper receipt.payload.deal_hash
    let mut tampered_deal_link = run.froglet_chain.clone();
    let orig_dh = tampered_deal_link[4]["payload"]["deal_hash"]
        .as_str()
        .expect("deal_hash string")
        .to_owned();
    tampered_deal_link[4]["payload"]["deal_hash"] = json!(flip_first_hex_nibble(&orig_dh));
    let (code_dh, rep_dh) = run_external_froglet_verify(
        &verify_bin,
        &tampered_deal_link,
        workspace.path(),
        "tampered_deal_hash.json",
    );
    assert_eq!(code_dh, 1);
    assert_eq!(rep_dh["valid"], json!(false));

    // 3d. Tamper receipt.signature
    let mut tampered_sig = run.froglet_chain.clone();
    let orig_sig = tampered_sig[4]["signature"]
        .as_str()
        .expect("signature string")
        .to_owned();
    tampered_sig[4]["signature"] = json!(flip_first_hex_nibble(&orig_sig));
    let (code_sig, rep_sig) = run_external_froglet_verify(
        &verify_bin,
        &tampered_sig,
        workspace.path(),
        "tampered_signature.json",
    );
    assert_eq!(code_sig, 1);
    assert_eq!(rep_sig["valid"], json!(false));

    // -------------------------------------------------------------------------
    // 4. Container Non-Zero Exit (`exit 2`) -> ACT Refund + Valid `failed` Chain
    // -------------------------------------------------------------------------
    // Running hla-motif-enrichment without a dataset_grant omits /in/data/cohort.parquet,
    // causing analyze.py inside the real container to print an error and exit with code 2.
    let balance_before_fail = ledger
        .available_balance(&org_id)
        .await
        .expect("balance before failing run");
    assert_eq!(balance_before_fail, CreditAmountAct::from_whole_act(45));

    let fail_quote = runtime
        .prepare_run(PrepareRunRequest {
            org_id: org_id.clone(),
            session_id: Some(session.session_id.clone()),
            plugin_id,
            params: json!({
                "alleles": ["HLA-A*02:01"],
                "min_length": 8,
                "max_length": 11
            }),
            dataset_grant: None,
        })
        .await
        .expect("prepare failing run");

    let failed_run = runtime
        .confirm_run(&fail_quote.run_id, "idem-gate3-fail-refund-002")
        .await
        .expect("confirm_run records terminal Failed RunRecord + refunds ACT");

    assert_eq!(failed_run.status, RunStatus::Failed);
    assert!(failed_run.receipt_verified);
    assert!(
        failed_run
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("non-zero code 2"),
        "expected exit code 2 failure message, got {:?}",
        failed_run.error_message
    );

    // ACT balance MUST be 100% restored to 45.00 ACT!
    let balance_after_refund = ledger
        .available_balance(&org_id)
        .await
        .expect("balance after refund");
    assert_eq!(
        balance_after_refund, balance_before_fail,
        "reserved ACT was not refunded after container exit 2!"
    );

    let txs = ledger
        .list_transactions(&org_id)
        .await
        .expect("list ledger transactions");
    assert!(
        txs.iter()
            .any(|tx| tx.run_id.as_ref() == Some(&fail_quote.run_id)
                && tx.kind == LedgerTxKind::RunRefund),
        "expected explicit RunRefund ledger entry for failed container run"
    );

    // Even the failed run's 5-artifact Froglet chain (`deal_state = "failed"`) MUST pass external froglet-verify!
    let (fail_chain_code, fail_chain_rep) = run_external_froglet_verify(
        &verify_bin,
        &failed_run.froglet_chain,
        workspace.path(),
        "failed_run_valid_chain.json",
    );
    assert_eq!(
        fail_chain_code, 0,
        "failed run chain should still be cryptographically valid: {fail_chain_rep}"
    );
    assert_eq!(fail_chain_rep["valid"], json!(true));
    assert_eq!(
        failed_run.froglet_chain[4]["payload"]["deal_state"],
        json!("failed")
    );
    assert_eq!(
        failed_run.froglet_chain[4]["payload"]["failure_code"],
        json!("EXIT_CODE_2")
    );
}
