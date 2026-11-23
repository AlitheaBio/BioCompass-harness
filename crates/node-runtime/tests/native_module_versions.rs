//! Local version resolution with isolated SQLite, real Froglet signing, and a fixture OCI CLI.
#![cfg(unix)]

use biocompass_core_traits::{
    CreditAmountAct, LocalFsBlobStore, OrgId, PaymentGateway, PluginId, PrepareRunRequest,
    RunDispatcher, RunStatus, SessionStore, SqliteActLedger, SqliteSessionStore,
    StandardOciPolicyVerifier,
};
use biocompass_node_runtime::{InProcessNodeRuntime, NodeKeypairSet, NodeRuntimeDeps};
use biocompass_oci_runner::{DetectedEngine, EngineKind, OciContainerRunner};
use biocompass_plugin_spec::LoadedPlugin;
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, path::Path, sync::Arc};
use tempfile::tempdir;

fn runtime(dir: &Path) -> InProcessNodeRuntime {
    let engine_path = dir.join("echo-runner");
    // Return the marker selected by the image argument supplied to the actual runner.
    fs::write(
        &engine_path,
        r#"#!/bin/sh
set -eu
if [ "$1" = version ]; then
  printf 'linux/amd64\n'
  exit 0
fi
if [ "$1" = image ] && [ "${2:-}" = inspect ]; then
  printf '[{"RepoDigests":["%s"],"Os":"linux","Architecture":"amd64"}]\n' "$3"
  exit 0
fi
[ "$1" = run ] || exit 0
for arg do
  case "$arg" in
    *:/out:rw) out=$(printf '%s' "$arg" | sed 's|:/out:rw$||') ;;
    *@sha256:1111*) number=17 ;;
    *@sha256:2222*) number=29 ;;
  esac
done
printf '{"number":%s}\n' "$number" > "$out/result.json"
"#,
    )
    .unwrap();
    fs::set_permissions(&engine_path, fs::Permissions::from_mode(0o700)).unwrap();
    InProcessNodeRuntime::new(NodeRuntimeDeps {
        keys: NodeKeypairSet::load_or_generate(dir.join("keys")).unwrap(),
        session_store: Arc::new(SqliteSessionStore::open(dir.join("sessions.sqlite")).unwrap()),
        payment_gateway: Arc::new(
            SqliteActLedger::open(
                dir.join("ledger.sqlite"),
                CreditAmountAct::from_whole_act(20),
            )
            .unwrap(),
        ),
        blob_store: Arc::new(LocalFsBlobStore::new(dir.join("store")).unwrap()),
        oci_registry: Arc::new(StandardOciPolicyVerifier::strict_pinned()),
        oci_runner: Arc::new(OciContainerRunner::from_engine(DetectedEngine {
            binary_path: engine_path,
            kind: EngineKind::Docker,
            rootless: true,
            server_version: "fixture".into(),
        })),
        stage_root_dir: dir.join("stages"),
        allow_local_dev_tags: false,
    })
    .unwrap()
}

fn release(root: &Path, major: u8, price: u64) -> LoadedPlugin {
    let dir = root.join(format!("v{major}"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("plugin.toml"),
        format!(
            r#"
id="fixture/number"
name="Number"
version="{major}.0.0"
execution="local"
network="none"
price_milli_act={price}
image="localhost/fixture/number@sha256:{}"
"#,
            major.to_string().repeat(64)
        ),
    )
    .unwrap();
    LoadedPlugin::load_from_dir(&dir, false).unwrap()
}

fn request(version: Option<&str>) -> PrepareRunRequest {
    PrepareRunRequest {
        org_id: OrgId::from("local-customer"),
        session_id: None,
        plugin_id: PluginId::from("fixture/number"),
        plugin_version: version.map(str::to_owned),
        params: json!({}),
        dataset_grant: None,
        offer_id: None,
    }
}

#[tokio::test]
async fn exact_release_and_existing_quote_survive_new_registration() {
    let dir = tempdir().unwrap();
    let node = runtime(dir.path());
    let one = release(dir.path(), 1, 2000);
    node.register_plugin(one.clone()).await.unwrap();
    let old_quote = node.prepare_run(request(None)).await.unwrap();
    node.register_plugin(release(dir.path(), 2, 7000))
        .await
        .unwrap();
    node.register_plugin(one.clone()).await.unwrap();
    let explicit = node.prepare_run(request(Some("1.0.0"))).await.unwrap();
    let latest = node.prepare_run(request(None)).await.unwrap();
    assert_eq!(
        (
            old_quote.plugin_version.as_str(),
            explicit.plugin_version.as_str(),
            latest.plugin_version.as_str()
        ),
        ("1.0.0", "1.0.0", "2.0.0")
    );
    assert_eq!(explicit.image_ref, old_quote.image_ref);
    assert_ne!(explicit.image_ref, latest.image_ref);
    assert_eq!(explicit.estimated_cost_act.milli_act(), 2000);
    assert_eq!(latest.estimated_cost_act.milli_act(), 7000);
    assert_eq!(node.list_plugins().await.len(), 1);
    assert_eq!(node.list_plugins().await[0].manifest.version, "2.0.0");
    for requested in ["1", "1.*", " ", "1.0.0 ", "9.0.0"] {
        assert!(
            node.prepare_run(request(Some(requested))).await.is_err(),
            "{requested:?}"
        );
    }
    for mutated in [
        {
            let mut p = one.clone();
            p.manifest.price_milli_act += 1000;
            p
        },
        {
            let mut p = one.clone();
            p.manifest.resources.cpus += 1;
            p
        },
        {
            let mut p = one;
            p.manifest.image = latest.image_ref.clone();
            p
        },
    ] {
        assert!(node.register_plugin(mutated).await.is_err());
    }
    for (quote, marker) in [(old_quote, 17), (explicit, 17), (latest, 29)] {
        let key = format!("approval-{}", quote.run_id);
        let run = node.confirm_run(&quote.run_id, &key).await.unwrap();
        assert_eq!(run.status, RunStatus::Succeeded, "{:?}", run.error_message);
        assert!(run.receipt_verified);
        assert_eq!(run.result_json, Some(json!({"number":marker})));
        assert_eq!(
            run.froglet_receipt,
            node.confirm_run(&quote.run_id, &key)
                .await
                .unwrap()
                .froglet_receipt
        );
    }
    let ledger = SqliteActLedger::open(
        dir.path().join("ledger.sqlite"),
        CreditAmountAct::from_whole_act(20),
    )
    .unwrap();
    assert_eq!(
        ledger
            .available_balance(&OrgId::from("local-customer"))
            .await
            .unwrap()
            .milli_act(),
        9000
    );
}

#[tokio::test]
async fn altered_manifest_after_restart_cannot_be_confirmed_or_charged() {
    let dir = tempdir().unwrap();
    let original = runtime(dir.path());
    let one = release(dir.path(), 1, 2000);
    original.register_plugin(one.clone()).await.unwrap();
    let prepared = original.prepare_run(request(Some("1.0.0"))).await.unwrap();
    drop(original);

    let restarted = runtime(dir.path());
    let mut changed = one;
    changed.manifest.resources.cpus += 1;
    restarted.register_plugin(changed).await.unwrap();
    assert!(
        restarted
            .confirm_run(&prepared.run_id, "approval")
            .await
            .is_err()
    );
    let ledger = SqliteActLedger::open(
        dir.path().join("ledger.sqlite"),
        CreditAmountAct::from_whole_act(20),
    )
    .unwrap();
    let org = OrgId::from("local-customer");
    assert!(
        ledger
            .run_reservation(&org, &prepared.run_id)
            .await
            .unwrap()
            .is_none()
    );
    let store = SqliteSessionStore::open(dir.path().join("sessions.sqlite")).unwrap();
    assert_eq!(
        store.get_run(&prepared.run_id).await.unwrap().status,
        RunStatus::Prepared
    );
}

#[tokio::test]
async fn modified_price_in_stored_quote_cannot_reserve_credits() {
    let dir = tempdir().unwrap();
    let node = runtime(dir.path());
    node.register_plugin(release(dir.path(), 1, 2000))
        .await
        .unwrap();
    let quote = node.prepare_run(request(Some("1.0.0"))).await.unwrap();
    let store = SqliteSessionStore::open(dir.path().join("sessions.sqlite")).unwrap();
    let mut row = store.get_run(&quote.run_id).await.unwrap();
    row.quote.estimated_cost_act = CreditAmountAct::from_whole_act(7);
    store.save_run(&row).await.unwrap();
    assert!(node.confirm_run(&quote.run_id, "approval").await.is_err());
    let ledger = SqliteActLedger::open(
        dir.path().join("ledger.sqlite"),
        CreditAmountAct::from_whole_act(20),
    )
    .unwrap();
    assert!(
        ledger
            .run_reservation(&row.org_id, &quote.run_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn selected_cloud_only_release_is_visible_but_never_prepared_or_confirmed_locally() {
    use biocompass_core_traits::CompassError;
    use biocompass_plugin_spec::PluginExecutionPlane;
    let dir = tempdir().unwrap();
    let one = release(dir.path(), 1, 2000);
    let node = runtime(dir.path());
    node.register_plugin(one.clone()).await.unwrap();
    let held = node.prepare_run(request(Some("1.0.0"))).await.unwrap();
    drop(node);

    let restarted = runtime(dir.path());
    let mut hosted_only = one;
    hosted_only.manifest.execution = PluginExecutionPlane::CloudOnly;
    restarted.register_plugin(hosted_only).await.unwrap();
    restarted
        .register_plugin(release(dir.path(), 2, 7000))
        .await
        .unwrap();
    let id = PluginId::from("fixture/number");
    assert_eq!(
        restarted
            .get_plugin_version(&id, Some("1.0.0"))
            .await
            .unwrap()
            .manifest
            .execution,
        PluginExecutionPlane::CloudOnly
    );
    assert_eq!(
        restarted
            .prepare_run(request(None))
            .await
            .unwrap()
            .plugin_version,
        "2.0.0"
    );
    assert!(matches!(
        restarted.prepare_run(request(Some("1.0.0"))).await,
        Err(CompassError::AccessDenied(_))
    ));
    assert!(matches!(
        restarted.confirm_run(&held.run_id, "original-key").await,
        Err(CompassError::AccessDenied(_))
    ));
    let ledger = SqliteActLedger::open(
        dir.path().join("ledger.sqlite"),
        CreditAmountAct::from_whole_act(20),
    )
    .unwrap();
    assert!(
        ledger
            .run_reservation(&OrgId::from("local-customer"), &held.run_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn private_dataset_without_observations_is_rejected_before_quote_or_access() {
    use biocompass_core_traits::{CatalogId, CompassError, DatasetGrant};
    use biocompass_data_engine::DuckLakeCatalogEngine;

    let scratch = tempdir().unwrap();
    let dir = scratch.path();
    let node = runtime(dir);
    let catalog_id = CatalogId::from("bc-private-v1/roster-only");
    let catalog = DuckLakeCatalogEngine::create_local_catalog(
        &dir.join("private"),
        catalog_id.clone(),
        "Private synthetic roster",
        "1",
        "Synthetic fixture without observations",
        "Synthetic",
        std::collections::BTreeMap::new(),
        "Initial publication",
        |conn| {
            conn.execute_batch(
                "CREATE TABLE dl.obs_samples AS SELECT 'S1' AS original_sample_number;",
            )
            .map_err(|e| CompassError::Storage(e.to_string()))
        },
    )
    .unwrap();
    let snapshot_id = catalog.summary().latest_snapshot_id;
    node.register_catalog(catalog).await.unwrap();
    let plugin = dir.join("target-validation");
    fs::create_dir(&plugin).unwrap();
    let module_hash = "1".repeat(64);
    fs::write(
        plugin.join("plugin.toml"),
        format!(
            r#"
id="alithea/bc-target-validation"
name="Target Validation"
version="2.1.0"
execution="local"
network="none"
dataset_input_contract="biocompass.target-validation.v1"
image="localhost/fixture/target-validation@sha256:{module_hash}"
"#
        ),
    )
    .unwrap();
    let plugin = LoadedPlugin::load_from_dir(plugin, false).unwrap();
    let plugin_id = plugin.manifest.id.clone();
    node.register_plugin(plugin).await.unwrap();
    node.approve_curated_module(plugin_id.clone(), "2.1.0", module_hash)
        .await;
    let org = OrgId::from("local-customer");
    let run = |grant| PrepareRunRequest {
        org_id: org.clone(),
        session_id: None,
        plugin_id: plugin_id.clone(),
        plugin_version: Some("2.1.0".into()),
        params: json!({
            "search_type": "peptide",
            "search_term": "GILGFVFTL",
            "dataset_version": 1,
            "cohort_filter": {"snapshot_id": snapshot_id}
        }),
        dataset_grant: Some(grant),
        offer_id: None,
    };
    assert!(matches!(
        node.prepare_run(run(DatasetGrant::full_access(org.clone(), catalog_id.clone())))
            .await,
        Err(CompassError::AccessDenied(_))
    ));
    assert!(matches!(
        node.prepare_run(run(DatasetGrant::compute_only_partner(org.clone(), catalog_id)))
            .await,
        Err(CompassError::Validation(reason)) if reason.contains("obs_sample_peptide_mappings")
    ));
    let store = SqliteSessionStore::open(dir.join("sessions.sqlite")).unwrap();
    assert!(store.list_runs(&org).await.unwrap().is_empty());
}
