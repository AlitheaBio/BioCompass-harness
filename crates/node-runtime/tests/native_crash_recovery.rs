//! Native recovery regression. Generates signed quotes and independent SQLite connections
//! in a temporary workspace. Recovery must succeed without any OCI executable or network.
use std::fs;
use std::path::Path;
use std::sync::Arc;

use biocompass_core_traits::{
    CreditAmountAct, LedgerTxKind, LocalFsBlobStore, OrgId, PaymentGateway, PluginId,
    PrepareRunRequest, RunDispatcher, RunId, RunStatus, SessionEventKind, SessionStore,
    SqliteActLedger, SqliteSessionStore, StandardOciPolicyVerifier,
};
use biocompass_node_runtime::{InProcessNodeRuntime, NodeKeypairSet, NodeRuntimeDeps};
use biocompass_oci_runner::{DetectedEngine, EngineKind, OciContainerRunner};
use biocompass_plugin_spec::LoadedPlugin;
use serde_json::json;
use tempfile::tempdir;

struct Native {
    runtime: InProcessNodeRuntime,
    store: Arc<SqliteSessionStore>,
    ledger: Arc<SqliteActLedger>,
}

impl Native {
    fn open(dir: &Path) -> Self {
        Self::with_engine(dir, dir.join("no-execution-allowed-during-recovery"))
    }

    fn with_engine(dir: &Path, binary_path: std::path::PathBuf) -> Self {
        let store = Arc::new(SqliteSessionStore::open(dir.join("sessions.sqlite")).unwrap());
        let ledger = Arc::new(
            SqliteActLedger::open(
                dir.join("ledger.sqlite"),
                CreditAmountAct::from_whole_act(10),
            )
            .unwrap(),
        );
        let engine = DetectedEngine {
            kind: EngineKind::Docker,
            binary_path,
            server_version: "offline fixture".into(),
            rootless: true,
        };
        let runtime = InProcessNodeRuntime::new(NodeRuntimeDeps {
            keys: NodeKeypairSet::load_or_generate(dir.join("keys")).unwrap(),
            session_store: store.clone(),
            payment_gateway: ledger.clone(),
            blob_store: Arc::new(LocalFsBlobStore::new(dir.join("store")).unwrap()),
            oci_registry: Arc::new(StandardOciPolicyVerifier::workstation_dev()),
            oci_runner: Arc::new(OciContainerRunner::from_engine(engine)),
            stage_root_dir: dir.join("stages"),
            allow_local_dev_tags: true,
        })
        .unwrap();
        Self {
            runtime,
            store,
            ledger,
        }
    }

    async fn prepare(&self, dir: &Path, org: &OrgId) -> RunId {
        self.prepare_with_timeout(
            dir,
            org,
            biocompass_plugin_spec::PluginResources::default().timeout_sec,
        )
        .await
    }

    async fn prepare_with_timeout(&self, dir: &Path, org: &OrgId, seconds: u64) -> RunId {
        let plugin_dir = dir.join("echo-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
id = "example/echo"
name = "Echo"
version = "1.0.0"
execution = "local"
network = "none"
price_milli_act = 2000
image = "localhost/example/echo@sha256:1111111111111111111111111111111111111111111111111111111111111111"

[resources]
timeout_sec = TIMEOUT
"#.replace("TIMEOUT", &seconds.to_string()),
        )
        .unwrap();
        let plugin = LoadedPlugin::load_from_dir(&plugin_dir, true).unwrap();
        self.runtime.register_plugin(plugin).await.unwrap();
        let session = self
            .store
            .create_session(org, "Recovery", "none")
            .await
            .unwrap();
        self.runtime
            .prepare_run(PrepareRunRequest {
                org_id: org.clone(),
                session_id: Some(session.session_id),
                plugin_id: PluginId::from("example/echo"),
                plugin_version: None,
                offer_id: None,
                params: json!({"sample": "one"}),
                dataset_grant: None,
                inputs: Vec::new(),
                parent_run_id: None,
                agent_credential_id: None,
                agent_tool_call_id: None,
                doc_id: None,
                doc_version: None,
            })
            .await
            .unwrap()
            .run_id
    }
}

#[tokio::test]
async fn startup_preserves_a_prepared_quote_that_was_never_approved() {
    let dir = tempdir().unwrap();
    let org = OrgId::from("org-regression");
    let original = Native::open(dir.path());
    let rid = original.prepare(dir.path(), &org).await;
    let before = original.store.get_run(&rid).await.unwrap();
    drop(original);

    let restarted = Native::open(dir.path());
    restarted
        .runtime
        .recover_interrupted_run(&rid)
        .await
        .unwrap();
    let after = restarted.store.get_run(&rid).await.unwrap();
    assert_eq!(
        serde_json::to_value(after).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    assert!(
        restarted
            .ledger
            .run_reservation(&org, &rid)
            .await
            .unwrap()
            .is_none()
    );
}

async fn abandoned_reservation(legacy_running_row: bool) {
    let dir = tempdir().unwrap();
    let org = OrgId::from("org-regression");
    let original = Native::open(dir.path());
    let rid = original.prepare(dir.path(), &org).await;
    let mut original_row = original.store.get_run(&rid).await.unwrap();
    let session = original_row.session_id.clone().unwrap();
    let price = original_row.quote.estimated_cost_act;
    assert_eq!(price, CreditAmountAct::from_whole_act(2));
    let key = "original-approval";
    original
        .ledger
        .reserve_for_run(&org, &rid, price, key)
        .await
        .unwrap();
    if legacy_running_row {
        original_row.status = RunStatus::Running;
        original.store.save_run(&original_row).await.unwrap();
    }
    drop(original);

    let first = Native::open(dir.path());
    let second = Native::open(dir.path());
    let mut first_notices = first.runtime.subscribe_terminal();
    let mut second_notices = second.runtime.subscribe_terminal();
    let (startup_one, startup_two) = tokio::join!(
        first.runtime.recover_interrupted_run(&rid),
        second.runtime.recover_interrupted_run(&rid)
    );
    startup_one.unwrap();
    startup_two.unwrap();
    let (left, right) = tokio::join!(
        first.runtime.confirm_run(&rid, key),
        second.runtime.confirm_run(&rid, key)
    );
    let left = left.unwrap();
    let notices: Vec<_> = [&mut first_notices, &mut second_notices]
        .into_iter()
        .flat_map(|receiver| std::iter::from_fn(|| receiver.try_recv().ok()))
        .collect();
    assert_eq!(notices.len(), 1, "only the process that published the outcome emits a notice");
    assert_eq!(notices[0].org_id, org);
    assert_eq!(notices[0].run_id, rid);
    assert_eq!(notices[0].status, first.store.get_run(&rid).await.unwrap().status);
    assert_eq!(left.status, RunStatus::Failed);
    assert!(left.result_json.is_none() && left.artifacts.is_empty());
    assert_eq!(
        serde_json::to_value(&left).unwrap(),
        serde_json::to_value(right.unwrap()).unwrap()
    );
    assert!(
        second
            .runtime
            .confirm_run(&rid, "different-approval")
            .await
            .is_err()
    );
    drop(first);
    drop(second);

    let restarted = Native::open(dir.path());
    restarted
        .runtime
        .recover_interrupted_run(&rid)
        .await
        .unwrap();
    let final_row = restarted.runtime.confirm_run(&rid, key).await.unwrap();
    assert_eq!(
        serde_json::to_value(&left).unwrap(),
        serde_json::to_value(final_row).unwrap()
    );
    let kinds: Vec<_> = restarted
        .ledger
        .list_transactions(&org)
        .await
        .unwrap()
        .into_iter()
        .filter(|t| t.run_id.as_ref() == Some(&rid))
        .map(|t| t.kind)
        .collect();
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == LedgerTxKind::RunReserve)
            .count(),
        1
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == LedgerTxKind::RunRefund)
            .count(),
        1
    );
    assert_eq!(kinds.len(), 2);
    assert_eq!(
        restarted.ledger.available_balance(&org).await.unwrap(),
        CreditAmountAct::from_whole_act(10)
    );
    let events = restarted.store.list_events(&session).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == SessionEventKind::RunCompleted
                && e.metadata["run_id"] == rid.as_str())
            .count(),
        1
    );
}

#[tokio::test]
async fn crash_between_reservation_and_running_is_refunded_once() {
    abandoned_reservation(false).await;
}

#[tokio::test]
async fn legacy_running_record_without_signed_output_is_refunded_once() {
    abandoned_reservation(true).await;
}

#[cfg(unix)]
fn fixture_engine(dir: &Path, behavior: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("test-engine");
    let script = r#"#!/bin/sh
set -eu
dir=${0%/*}
case "$1" in
    version) echo linux/arm64 ;;
    image) printf '[{"RepoDigests":["%s"],"Os":"linux","Architecture":"arm64"}]\n' "$3" ;;
    TEST_BEHAVIOR
    *) exit 0 ;;
esac
"#;
    fs::write(&path, script.replace("TEST_BEHAVIOR", behavior)).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

/// The engine CLI accepts the run but stops responding to both `run` and cleanup.
/// Observe the original wallet independently of the supervisor's in-memory view.
#[cfg(unix)]
#[tokio::test]
async fn paid_timeout_refunds_once_while_engine_cleanup_is_unresponsive() {
    use std::time::{Duration, Instant};

    let dir = tempdir().unwrap();
    let engine = fixture_engine(
        dir.path(),
        r#"
        run)
            for argument in "$@"; do
                case "$argument" in *:/out:rw) out=${argument%:/out:rw} ;; esac
            done
            mkdir -p "$out/artifacts"
            printf '{"appears_valid":true}' > "$out/result.json"
            printf 'value\n42\n' > "$out/artifacts/preview.csv"
            echo run >> "$dir/engine-events"; exec sleep 20 ;;
        rm) echo rm >> "$dir/engine-events"; exec sleep 20 ;;
        "#,
    );
    let org = OrgId::from("org-timeout");
    let supervisor = Native::with_engine(dir.path(), engine.clone());
    let rid = supervisor.prepare_with_timeout(dir.path(), &org, 1).await;
    let key = "approved-timeout-only-once";
    let inspection = rusqlite::Connection::open_with_flags(
        dir.path().join("ledger.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let balance = || {
        let sql =
            "SELECT available_milli_act, reserved_milli_act FROM org_balances WHERE org_id=?1";
        inspection
            .query_row(sql, [org.as_str()], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .unwrap()
    };
    let started = Instant::now();
    let failed = tokio::time::timeout(Duration::from_secs(12), async {
        let confirm = supervisor.runtime.confirm_run(&rid, key);
        tokio::pin!(confirm);
        let observe_reserved = async {
            while !dir.path().join("engine-events").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(balance(), (8_000, 2_000));
            eprintln!(
                "Native engine accepted run {}; observed SQLite 8000 available/2000 reserved",
                rid.as_str()
            );
        };
        tokio::select! {
            done = &mut confirm => panic!("confirmation ended before engine execution: {done:?}"),
            () = observe_reserved => (),
        }
        confirm.await.unwrap()
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "unresponsive engine held the reservation: {:?}, engine commands: {:?}",
            balance(),
            fs::read_to_string(dir.path().join("engine-events"))
        )
    });
    assert_eq!(failed.status, RunStatus::Failed);
    let reason = failed.error_message.as_deref().unwrap();
    assert!(
        reason.contains("CONTAINER_TIMEOUT") && reason.contains("OCI_CLEANUP_UNCONFIRMED"),
        "{reason}"
    );
    assert!(failed.receipt_verified && failed.froglet_receipt.is_some());
    assert!(failed.result_json.is_none() && failed.artifacts.is_empty());
    assert_eq!(balance(), (10_000, 0));
    drop(supervisor);

    let restarted = Native::with_engine(dir.path(), engine);
    restarted
        .runtime
        .recover_interrupted_run(&rid)
        .await
        .unwrap();
    let retried = restarted.runtime.confirm_run(&rid, key).await.unwrap();
    assert_eq!(
        serde_json::to_value(&failed).unwrap(),
        serde_json::to_value(retried).unwrap()
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("engine-events")).unwrap(),
        "run\nrm\n"
    );
    assert_eq!(balance(), (10_000, 0));
    let blobs = LocalFsBlobStore::new(dir.path().join("store")).unwrap();
    for suffix in ["result.json", "artifacts/preview.csv"] {
        let key = format!("runs/{}/{suffix}", rid.as_str());
        assert!(
            !biocompass_core_traits::BlobStore::exists(&blobs, &key)
                .await
                .unwrap(),
            "timeout published an unverified staged output {key}"
        );
    }
    let mut stmt = inspection.prepare(
        "SELECT json_extract(kind,'$'),delta_milli_act FROM ledger_txs WHERE org_id=?1 AND run_id=?2 ORDER BY kind"
    ).unwrap();
    let entries: Vec<(String, i64)> = stmt
        .query_map([org.as_str(), rid.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        entries,
        [
            ("run_refund".to_owned(), 2_000),
            ("run_reserve".to_owned(), -2_000)
        ]
    );
    eprintln!(
        "Paid timeout {} finished in {:.3}s. Original SQLite wallet 10000 available/0 reserved after recovery and retry. Journal {:?}.",
        rid.as_str(),
        started.elapsed().as_secs_f64(),
        entries
    );
}

#[cfg(unix)]
#[tokio::test]
async fn startup_cleanup_skips_live_confirmations_and_unknown_run_ids() {
    use std::time::Duration;
    let dir = tempdir().unwrap();
    let engine = fixture_engine(
        dir.path(),
        r#"
        run)
            shift
            while [ "$#" -gt 0 ]; do
                case "$1" in
                    --name) shift; name=$1 ;;
                    *:/out:rw) out=${1%:/out:rw} ;;
                esac
                shift
            done
            printf '%s' "$name" > "$dir/live-name.tmp"
            mv "$dir/live-name.tmp" "$dir/live-name"
            while [ ! -f "$dir/allow-finish" ]; do sleep 0.02; done
            printf '{"finished":true}' > "$out/result.json" ;;
        ps) cat "$dir/inventory" ;;
        rm) printf '%s\n' "$3" >> "$dir/removed" ;;
    "#,
    );
    let first = Native::with_engine(dir.path(), engine.clone());
    let second = Native::with_engine(dir.path(), engine);
    let org = OrgId::from("org-run-owner");
    let rid = first.prepare(dir.path(), &org).await;
    let known = "a".repeat(64);
    let other = "b".repeat(64);
    let live = dir.path().join("live-name");
    let removed = dir.path().join("removed");
    let confirm = first.runtime.confirm_run(&rid, "approved-by-owner");
    tokio::pin!(confirm);
    let sweep = async {
        while !live.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let name = fs::read_to_string(live).unwrap();
        assert_eq!(
            first.store.get_run(&rid).await.unwrap().status,
            RunStatus::Running
        );
        let prefix = name.strip_suffix(&hex::encode(rid.as_str())).unwrap();
        let unknown = RunId::new("run-ffffffffffffffffffffffffffffffff".to_owned()).unwrap();
        let listing = format!(
            "{known} {name}\n{other} {prefix}{}\n",
            hex::encode(unknown.as_str())
        );
        fs::write(dir.path().join("inventory"), listing).unwrap();
        second.runtime.recover_abandoned_containers().await.unwrap();
        assert!(!removed.exists(), "startup removed a live or unknown run");
        fs::write(dir.path().join("allow-finish"), b"finish").unwrap();
    };
    let finished=tokio::time::timeout(Duration::from_secs(10),async {
        tokio::select! {
            done = &mut confirm => panic!("expected to observe a live leased execution, found {done:?}"),
            () = sweep => (),
        }
        confirm.await.unwrap()
    }).await.unwrap();
    assert_eq!(
        finished.status,
        RunStatus::Succeeded,
        "{:?}",
        finished.error_message
    );
    second.runtime.recover_abandoned_containers().await.unwrap();
    assert_eq!(fs::read_to_string(removed).unwrap(), format!("{known}\n"));
}
