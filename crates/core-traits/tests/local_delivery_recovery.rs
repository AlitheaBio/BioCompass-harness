#![cfg(feature = "local-sqlite")]

use biocompass_core_traits::local_delivery::{DeliveryFile, DeliveryScope, LocalDeliveryStore};
use biocompass_core_traits::{CompassError, OrgId, RunRecord};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn scope() -> DeliveryScope {
    DeliveryScope {
        origin: "https://cloud.invalid".into(),
        org_id: OrgId::from("buyer"),
        subject: "scientist".into(),
    }
}

fn run() -> RunRecord {
    serde_json::from_value(json!({
        "run_id":"r1","org_id":"buyer","session_id":null,"plugin_id":"measure","plugin_version":"1",
        "status":"succeeded","params":{},"grant":null,"result_json":{"value":1},
        "quote":{"run_id":"r1","plugin_id":"measure","plugin_version":"1","image_ref":"image@sha256:abc",
            "target_node_id":"worker","estimated_cost_milli_eur":17,"workload_hash":hash(b"work"),
            "dataset_grant_summary":null,"froglet_quote":{"signer":"provider"},"expires_at":"2026-01-01T12:00:00Z"},
        "artifacts":[{"name":"nested/raw.dat","blob_key":"remote/external","media_type":"application/octet-stream",
            "sha256":hash(b"binary\x00"),"size_bytes":7}],
        "froglet_receipt":{"hash":hash(b"receipt")},"froglet_chain":[],"receipt_verified":true,
        "logs":[],"error_message":null,"created_at":"2026-01-01T12:00:00Z","completed_at":"2026-01-01T12:10:00Z"
    })).unwrap()
}

const RAW: &[u8] = b"{  \"value\" : 1 }\n";

async fn publish(
    store: &LocalDeliveryStore,
    scope: &DeliveryScope,
    run: &RunRecord,
    file: &[u8],
) -> Result<biocompass_core_traits::local_delivery::LocalDelivery, CompassError> {
    store
        .publish(
            scope,
            run,
            &hash(b"receipt"),
            &hash(RAW),
            &json!([{"snapshot_id":9}]),
            RAW,
            &[DeliveryFile {
                name: run.artifacts[0].name.clone(),
                bytes: file.to_vec(),
            }],
        )
        .await
}

#[tokio::test]
async fn restart_retry_preserves_one_result_with_exact_bytes_and_acknowledgement() {
    let dir = tempfile::tempdir().unwrap();
    let (scope, run) = (scope(), run());
    assert!(LocalDeliveryStore::open_workspace_existing(dir.path()).is_err());
    assert!(!dir.path().join("sessions.sqlite").exists());

    let store = LocalDeliveryStore::open_workspace(dir.path()).unwrap();
    let id = store.workspace_id().to_owned();
    store.register_pending(&scope, &run.quote).await.unwrap();
    assert!(matches!(
        publish(&store, &scope, &run, b"broken!").await,
        Err(CompassError::IntegrityViolation(_))
    ));
    assert!(store.list(&scope).await.unwrap().is_empty());
    // The independently persisted result object is invisible until all its artifacts commit.
    assert!(
        dir.path()
            .join("store/workspace-result-objects")
            .join(hash(RAW))
            .exists()
    );
    drop(store);
    let first = LocalDeliveryStore::open_workspace(dir.path()).unwrap();
    let second = LocalDeliveryStore::open_workspace(dir.path()).unwrap();
    assert_eq!(id, first.workspace_id());
    let (a, b) = tokio::join!(
        publish(&first, &scope, &run, b"binary\x00"),
        publish(&second, &scope, &run, b"binary\x00")
    );
    assert_eq!(a.unwrap().result_sha256, b.unwrap().result_sha256);
    drop((first, second)); // lose both response and in-memory state
    let store = LocalDeliveryStore::open_workspace(dir.path()).unwrap();
    let pending = store.pending(&scope).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].saved && !pending[0].acknowledged);
    assert_eq!(store.list(&scope).await.unwrap().len(), 1);
    assert_eq!(store.result_bytes(&scope, &run.run_id).await.unwrap(), RAW);
    let changed = RunRecord {
        logs: vec!["unexpected".into()],
        ..run.clone()
    };
    assert!(matches!(
        publish(&store, &scope, &changed, b"binary\x00").await,
        Err(CompassError::IntegrityViolation(_))
    ));
    for other in [
        DeliveryScope {
            org_id: OrgId::from("another"),
            ..scope.clone()
        },
        DeliveryScope {
            subject: "another".into(),
            ..scope.clone()
        },
        DeliveryScope {
            origin: "https://different.invalid".into(),
            ..scope.clone()
        },
    ] {
        assert!(matches!(
            store.result_bytes(&other, &run.run_id).await,
            Err(CompassError::NotFound(_))
        ));
        assert!(store.list(&other).await.unwrap().is_empty());
        assert!(store.pending(&other).await.unwrap().is_empty());
    }
    let object = dir
        .path()
        .join("store/workspace-result-objects")
        .join(hash(b"binary\x00"));
    std::fs::write(&object, b"damaged").unwrap();
    assert!(store.verify_saved(&scope, &run.run_id).await.is_err());
    publish(&store, &scope, &run, b"binary\x00").await.unwrap();
    store.verify_saved(&scope, &run.run_id).await.unwrap();
    store
        .acknowledge(&scope, &run.run_id, &hash(b"receipt"))
        .await
        .unwrap();
    store
        .acknowledge(&scope, &run.run_id, &hash(b"receipt"))
        .await
        .unwrap();
    assert!(store.pending(&scope).await.unwrap().is_empty());
    drop(store);
    let offline = LocalDeliveryStore::open_workspace_existing(dir.path()).unwrap();
    assert_eq!(offline.workspace_id(), id);
    assert_eq!(
        offline
            .artifact_bytes(&scope, &run.run_id, "nested/raw.dat")
            .await
            .unwrap(),
        b"binary\x00"
    );
    assert!(
        offline
            .get_pending(&scope, &run.run_id)
            .await
            .unwrap()
            .acknowledged
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&object).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(object.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

#[tokio::test]
async fn conflicting_quote_and_retired_run_cannot_publish() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalDeliveryStore::open_workspace(dir.path()).unwrap();
    let (scope, mut run) = (scope(), run());
    store.register_pending(&scope, &run.quote).await.unwrap();
    let changed = biocompass_core_traits::RunQuote {
        froglet_quote: Value::Null,
        ..run.quote.clone()
    };
    assert!(store.register_pending(&scope, &changed).await.is_err());
    let other = DeliveryScope {
        subject: "different".into(),
        ..scope.clone()
    };
    assert!(store.register_pending(&other, &run.quote).await.is_err());
    run.artifacts[0].name = "../outside".into();
    assert!(publish(&store, &scope, &run, b"binary\x00").await.is_err());
    store
        .retire_without_result(&scope, &run.run_id)
        .await
        .unwrap();
    assert!(store.pending(&scope).await.unwrap().is_empty());
    assert!(
        store
            .get_pending(&scope, &run.run_id)
            .await
            .unwrap()
            .retired
    );
    assert!(store.list(&scope).await.unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn local_result_metadata_is_private_with_new_or_existing_sqlite_files() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o750)).unwrap();
    let first = LocalDeliveryStore::open_workspace(dir.path()).unwrap();
    let files = [
        "sessions.sqlite",
        "sessions.sqlite-wal",
        "sessions.sqlite-shm",
    ];
    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    for file in files {
        let path = dir.path().join(file);
        assert_eq!(mode(&path), 0o600);
        // Existing workspaces may have been created using the process-wide default mode.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let second = LocalDeliveryStore::open_workspace(dir.path()).unwrap();
    assert_eq!(first.workspace_id(), second.workspace_id());
    for file in files {
        assert_eq!(mode(&dir.path().join(file)), 0o600);
    }
    assert_eq!(mode(dir.path()), 0o750);
}

/// A real process exits between filesystem publication and the SQLite transaction.
/// Use WAL's independent writer to hold the commit without modifying production code.
#[tokio::test]
async fn process_loss_between_object_writes_and_local_commit_recovers() {
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    async fn wait_for(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {}",
                path.display()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    const CHILD_ROOT: &str = "BIOCOMPASS_TEST_WORKSPACE_PICKUP_ROOT";
    let (scope, run) = (scope(), run());
    if let Some(dir) = std::env::var_os(CHILD_ROOT) {
        let dir = Path::new(&dir);
        let store = LocalDeliveryStore::open_workspace(dir).unwrap();
        std::fs::write(dir.join("child-ready"), []).unwrap();
        wait_for(&dir.join("publish")).await;
        publish(&store, &scope, &run, b"binary\x00").await.unwrap();
        panic!("parent must interrupt the child before SQLite can commit");
    }

    let dir = tempfile::tempdir().unwrap();
    let store = LocalDeliveryStore::open_workspace(dir.path()).unwrap();
    store.register_pending(&scope, &run.quote).await.unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process_loss_between_object_writes_and_local_commit_recovers",
        ])
        .env(CHILD_ROOT, dir.path())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(&dir.path().join("child-ready")).await;
    let conn = rusqlite::Connection::open(dir.path().join("sessions.sqlite")).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    std::fs::write(dir.path().join("publish"), []).unwrap();
    let objects = dir.path().join("store/workspace-result-objects");
    wait_for(&objects.join(hash(b"binary\x00"))).await;
    assert_eq!(std::fs::read(objects.join(hash(RAW))).unwrap(), RAW);
    assert_eq!(
        std::fs::read(objects.join(hash(b"binary\x00"))).unwrap(),
        b"binary\x00"
    );
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    conn.execute_batch("ROLLBACK").unwrap();
    drop((conn, store));
    let reopened = LocalDeliveryStore::open_workspace(dir.path()).unwrap();
    assert!(
        !reopened
            .get_pending(&scope, &run.run_id)
            .await
            .unwrap()
            .saved
    );
    assert!(reopened.list(&scope).await.unwrap().is_empty());
    publish(&reopened, &scope, &run, b"binary\x00")
        .await
        .unwrap();
    assert_eq!(reopened.list(&scope).await.unwrap().len(), 1);
    assert_eq!(
        reopened.result_bytes(&scope, &run.run_id).await.unwrap(),
        RAW
    );
    assert_eq!(
        reopened
            .artifact_bytes(&scope, &run.run_id, "nested/raw.dat")
            .await
            .unwrap(),
        b"binary\x00"
    );
}
