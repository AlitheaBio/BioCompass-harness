//! R11 regression test: clean-clone local execution falls back to building bundled `plugins/<slug>/Dockerfile`
//! when the pinned registry image is not cached locally and the registry is unreachable, while keeping
//! non-bundled plugins strictly fail-closed.
#![cfg(unix)]

use async_trait::async_trait;
use biocompass_core_traits::{
    CompassError, CompassResult, CreditAmountAct, DatasetGrant, LocalFsBlobStore, OciImageRef,
    OciRegistryClient, OrgId, PluginId, PrepareRunRequest, RunDispatcher, RunStatus,
    SqliteActLedger, SqliteSessionStore,
};
use biocompass_data_engine::{DuckDbCatalogEngine, seed_demo_hla_catalog};
use biocompass_node_runtime::{InProcessNodeRuntime, NodeKeypairSet, NodeRuntimeDeps};
use biocompass_oci_runner::{DetectedEngine, EngineKind, OciContainerRunner, OciDistributionClient};
use biocompass_plugin_spec::LoadedPlugin;
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, path::Path, sync::Arc};
use tempfile::tempdir;

struct CleanCloneVerifier {
    runner: Arc<OciContainerRunner>,
    registry: OciDistributionClient,
}

#[async_trait]
impl OciRegistryClient for CleanCloneVerifier {
    async fn verify_image(&self, image_raw: &str) -> CompassResult<OciImageRef> {
        let pin = OciImageRef::parse(image_raw, false)?;
        if self.runner.has_verified_local_digest(&pin.raw).await {
            return Ok(pin);
        }
        self.registry.verify_image(&pin.raw).await
    }
}

fn setup_clean_clone_runtime(dir: &Path) -> InProcessNodeRuntime {
    let engine_path = dir.join("fake-docker");
    let built_marker = dir.join("built-images.txt");
    fs::write(
        &engine_path,
        format!(
            r#"#!/bin/sh
set -eu
BUILT="{}"
touch "$BUILT"
case "${{1:-}}" in
  build)
    tag=""
    prev=""
    for arg do
      if [ "$prev" = "-t" ]; then tag="$arg"; fi
      prev="$arg"
    done
    [ -n "$tag" ] || exit 2
    printf '%s\n' "$tag" >> "$BUILT"
    exit 0
    ;;
  tag)
    src="${{2:-}}"
    dst="${{3:-}}"
    grep -Fxq "$src" "$BUILT" || [ "$src" = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" ] || exit 3
    printf '%s\n' "$dst" >> "$BUILT"
    exit 0
    ;;
  image)
    if [ "${{2:-}}" = "inspect" ]; then
      target=""
      for arg do target="$arg"; done
      if grep -Fxq "$target" "$BUILT"; then
        printf 'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n'
        exit 0
      fi
      exit 1
    fi
    exit 1
    ;;
  ps)
    exit 0
    ;;
  run)
    has_pull_never=0
    out=""
    for arg do
      case "$arg" in
        --pull=never) has_pull_never=1 ;;
        *:/out:rw) out=$(printf '%s' "$arg" | sed 's|:/out:rw$||') ;;
      esac
    done
    [ "$has_pull_never" = "1" ] || exit 41
    printf '{{"status":"ok","source":"bundled_dockerfile"}}\n' > "$out/result.json"
    exit 0
    ;;
  *)
    exit 1
    ;;
esac
"#,
            built_marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&engine_path, fs::Permissions::from_mode(0o700)).unwrap();

    let oci_runner = Arc::new(OciContainerRunner::from_engine(DetectedEngine {
        binary_path: engine_path,
        kind: EngineKind::Docker,
        rootless: false,
        server_version: "29.10.0-test".into(),
    }));
    let verifier = CleanCloneVerifier {
        runner: Arc::clone(&oci_runner),
        registry: OciDistributionClient::new(true).unwrap(),
    };

    InProcessNodeRuntime::new(NodeRuntimeDeps {
        keys: NodeKeypairSet::load_or_generate(dir.join("keys")).unwrap(),
        session_store: Arc::new(SqliteSessionStore::open(dir.join("sessions.sqlite")).unwrap()),
        payment_gateway: Arc::new(
            SqliteActLedger::open(
                dir.join("ledger.sqlite"),
                CreditAmountAct::from_whole_act(50),
            )
            .unwrap(),
        ),
        blob_store: Arc::new(LocalFsBlobStore::new(dir.join("store")).unwrap()),
        oci_registry: Arc::new(verifier),
        oci_runner,
        stage_root_dir: dir.join("stages"),
        allow_local_dev_tags: false,
    })
    .unwrap()
}

#[tokio::test]
async fn r11_clean_clone_bundled_plugin_builds_dockerfile_when_registry_unreachable() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let runtime = setup_clean_clone_runtime(root);

    let catalog_dir = root.join("catalogs/hla-immunopeptidomics-v1");
    seed_demo_hla_catalog(&catalog_dir).unwrap();
    let catalog = DuckDbCatalogEngine::open_verified(&catalog_dir).unwrap();
    let catalog_id = catalog.manifest().id.clone();
    runtime.register_catalog(catalog).await.unwrap();

    // 1. Bundled plugin under `<data_dir>/bundled-plugins/demo-bundled` with unreachable 127.0.0.1:1 registry.
    let bundled_dir = root.join("bundled-plugins/demo-bundled");
    fs::create_dir_all(&bundled_dir).unwrap();
    let pinned_digest = "1".repeat(64);
    fs::write(
        bundled_dir.join("plugin.toml"),
        format!(
            r#"
id = "alithea/demo-bundled"
name = "Demo Bundled Module"
version = "1.0.0"
execution = "either"
requires_catalog_level = "compute_only"
network = "none"
price_milli_act = 5000
image = "127.0.0.1:1/alithea/demo-bundled@sha256:{pinned_digest}"
"#
        ),
    )
    .unwrap();
    fs::write(
        bundled_dir.join("Dockerfile"),
        "FROM python:3.12-slim-bookworm\nUSER 65532:65532\n",
    )
    .unwrap();
    let bundled_plugin = LoadedPlugin::load_from_dir(&bundled_dir, false).unwrap();
    runtime.register_plugin(bundled_plugin).await.unwrap();
    runtime
        .approve_curated_module("alithea/demo-bundled".into(), "1.0.0", &pinned_digest)
        .await;

    // 2. Non-bundled user plugin pointing to unreachable registry MUST still fail closed.
    let user_dir = root.join("plugins/unbundled");
    fs::create_dir_all(&user_dir).unwrap();
    fs::write(
        user_dir.join("plugin.toml"),
        format!(
            r#"
id = "customer/unbundled"
name = "Unbundled Module"
version = "1.0.0"
execution = "local"
network = "none"
price_milli_act = 0
image = "127.0.0.1:1/customer/unbundled@sha256:{}"
"#,
            "2".repeat(64)
        ),
    )
    .unwrap();
    fs::write(user_dir.join("Dockerfile"), "FROM scratch\n").unwrap();
    let user_plugin = LoadedPlugin::load_from_dir(&user_dir, false).unwrap();
    runtime.register_plugin(user_plugin).await.unwrap();

    let org_id = OrgId::from("org-local-scientist");
    let unbundled_err = runtime
        .prepare_run(PrepareRunRequest {
            org_id: org_id.clone(),
            session_id: None,
            plugin_id: PluginId::from("customer/unbundled"),
            plugin_version: None,
            offer_id: None,
            params: json!({}),
            dataset_grant: None,
            inputs: Vec::new(),
            parent_run_id: None,
            agent_credential_id: None,
            agent_tool_call_id: None,
            doc_id: None,
            doc_version: None,
        })
        .await
        .expect_err("non-bundled plugin must fail closed when pinned registry is unreachable");
    assert!(matches!(
        unbundled_err,
        CompassError::ContainerEngine(_) | CompassError::NotFound(_) | CompassError::Storage(_)
    ));

    // 3. Bundled plugin prepares cleanly by building from bundled Dockerfile, surfaces a clear notice,
    //    preserves compute_only curated approval, and confirms with a verified 5-artifact Schnorr chain.
    let compute_only_grant = DatasetGrant::compute_only_partner(org_id.clone(), catalog_id);
    let quote = runtime
        .prepare_run(PrepareRunRequest {
            org_id,
            session_id: None,
            plugin_id: PluginId::from("alithea/demo-bundled"),
            plugin_version: None,
            offer_id: None,
            params: json!({}),
            dataset_grant: Some(compute_only_grant),
            inputs: Vec::new(),
            parent_run_id: None,
            agent_credential_id: None,
            agent_tool_call_id: None,
            doc_id: None,
            doc_version: None,
        })
        .await
        .expect("bundled plugin should build from bundled Dockerfile when registry is unreachable");

    assert!(
        quote.image_ref.starts_with("local/bundled-demo-bundled:"),
        "unexpected fallback image_ref: {}",
        quote.image_ref
    );
    let notice = quote
        .local_bundled_fallback_notice()
        .expect("expected local_bundled_fallback_notice on RunQuote when built locally");
    assert!(
        notice.contains("Dockerfile"),
        "expected notice to mention bundled Dockerfile fallback, got: {notice}"
    );

    let completed = runtime
        .confirm_run(&quote.run_id, "idem-r11-clean-clone")
        .await
        .expect("confirm bundled fallback run");
    assert_eq!(completed.status, RunStatus::Succeeded);
    assert!(completed.receipt_verified);
    assert_eq!(completed.froglet_chain.len(), 5);
    assert_eq!(
        completed.result_json,
        Some(json!({"status": "ok", "source": "bundled_dockerfile"}))
    );
    assert!(
        completed
            .logs
            .iter()
            .any(|line| line.contains("Dockerfile")),
        "expected run logs to record bundled Dockerfile fallback notice"
    );
}
