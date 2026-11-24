//! Native workbench entry point for the same versioned importer contract used on trusted nodes.
pub(crate) mod operator;
pub(crate) mod changes;
mod ownership;
pub(super) use ownership::{owner, sign_release_change, verify_release_change};
use crate::{
    dataset_store::LocalStore,
    server::ApiResult,
    state::{AppState, DeploymentMode},
};
use axum::{
    Json,
    extract::{Path as AxumPath, Query, Request, State},
};
use biocompass_core_traits::{
    CatalogId, CompassError, CompassResult, OciImageRef, PluginId, RunId,
};
use biocompass_data_engine::{DuckDbCatalogEngine, private_release};
use biocompass_node_runtime::InProcessNodeRuntime;
use biocompass_oci_runner::{ContainerRunSpec, prepare_stage_permissions};
use biocompass_plugin_spec::{DatasetImportContract, NetworkPolicy, PluginManifest};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, sync::Arc};
use tokio::io::AsyncWriteExt;

use changes::append_archive::MAX_BYTES;
const STORE: &str = "imported-private-datasets";

fn storage(error: impl std::fmt::Display) -> CompassError {
    CompassError::Storage(error.to_string())
}
fn store_error(error: String) -> CompassError {
    if error.starts_with("conflict:") {
        CompassError::IntegrityViolation(error)
    } else if error.starts_with("forbidden:") {
        CompassError::AccessDenied(error)
    } else if error.starts_with("not_found:") {
        CompassError::NotFound(error)
    } else if error.starts_with("forbidden:") {
        CompassError::AccessDenied(error)
    } else if error.starts_with("not_found:") {
        CompassError::NotFound(error)
    } else if error.starts_with("invalid:") {
        CompassError::Validation(error)
    } else {
        storage(error)
    }
}
fn local(state: &AppState) -> CompassResult<&Arc<InProcessNodeRuntime>> {
    if state.mode != DeploymentMode::Local {
        return Err(CompassError::AccessDenied(
            "installed importers require a native workspace".into(),
        ));
    }
    state
        .local_runtime
        .as_ref()
        .ok_or_else(|| CompassError::Internal("local runtime unavailable".into()))
}

struct Installed {
    manifest: PluginManifest,
    contract: DatasetImportContract,
    image: OciImageRef,
    _lease: Option<fs::File>,
}

async fn installed(state: &AppState, id: &PluginId, version: &str) -> CompassResult<Installed> {
    let (plugin, lease) = local(state)?
        .local_execution_plugin(id, Some(version))
        .await?;
    let manifest = plugin.manifest;
    let contract = manifest.dataset_import_contract()?;
    if !manifest.network.is_airgapped()
        || !manifest.price_milli_eur.is_zero()
        || manifest
            .offers
            .iter()
            .any(|offer| !offer.price_milli_eur.is_zero() || !offer.base_fee_milli_eur.is_zero())
    {
        return Err(CompassError::AccessDenied(
            "this upload requires a free, airgapped importer".into(),
        ));
    }
    let runner = state
        .oci_runner
        .as_ref()
        .ok_or_else(|| CompassError::ContainerEngine("local engine unavailable".into()))?;
    let image = runner
        .installed_execution_image(&manifest.image, state.allow_local_dev_tags)
        .await?;
    Ok(Installed {
        manifest,
        contract,
        image,
        _lease: lease,
    })
}

pub(crate) async fn discover(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let runtime = local(&state)?;
    let mut items = Vec::new();
    for active in runtime.list_plugins().await {
        for release in runtime.plugin_releases(&active.manifest.id).await {
            if release
                .manifest
                .inputs
                .schema
                .get("x-biocompass-import")
                .is_none()
            {
                continue;
            }
            match installed(&state, &release.manifest.id, &release.manifest.version).await {
                Ok(module) => items.push(json!({"plugin_id":module.manifest.id,"version":module.manifest.version,
                    "name":module.manifest.name,"accept":module.contract.accept,"row_corrections":false})),
                Err(CompassError::NotFound(_) | CompassError::Validation(_) | CompassError::AccessDenied(_)) => (),
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(Json(json!({"importers":items})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportQuery {
    filename: String,
    version: String,
    dataset_name: String,
    #[serde(default)]
    for_append: bool,
}

fn check_tree(path: &Path) -> CompassResult<()> {
    let mut pending = vec![path.to_owned()];
    while let Some(dir) = pending.pop() {
        for item in fs::read_dir(dir).map_err(storage)? {
            let item = item.map_err(storage)?;
            let kind = item.file_type().map_err(storage)?;
            if kind.is_dir() {
                pending.push(item.path());
            } else if !kind.is_file() {
                return Err(CompassError::IntegrityViolation(
                    "importer output contains a non-regular file".into(),
                ));
            }
        }
    }
    Ok(())
}

fn read_report(path: &Path) -> CompassResult<Value> {
    let meta = fs::metadata(path)
        .map_err(|_| CompassError::Validation("importer did not produce result.json".into()))?;
    if meta.len() > 32 * 1024 * 1024 {
        return Err(CompassError::Validation(
            "importer report exceeds 32 MiB".into(),
        ));
    }
    serde_json::from_reader(fs::File::open(path).map_err(storage)?).map_err(storage)
}

fn open_version(
    store: &LocalStore,
    dataset: &str,
    version: Option<u64>,
) -> CompassResult<Option<(DuckDbCatalogEngine, Value)>> {
    let family = match store.published_state(dataset) {
        Ok(s) => s,
        Err(e) if e.starts_with("not_found:") => return Ok(None),
        Err(e) => return Err(store_error(e)),
    };
    if family.release.is_none() { return Ok(None); }
    let release = family.version(version).map_err(store_error)?;
    open_committed(store, dataset, &family, release).map(Some)
}

fn open_committed(
    store: &LocalStore, dataset: &str, family: &crate::dataset_store::State,
    release: &crate::dataset_store::Release,
) -> CompassResult<(DuckDbCatalogEngine, Value)> {
    let path = store.dataset_directory(dataset).map_err(store_error)?.join(&release.object_dir);
    let verified = private_release::verify(&path).map_err(store_error)?;
    let base = format!("bc-private-v1/{dataset}");
    let expected = if release.version == 1 { base } else { format!("{base}/v{}", release.version) };
    if verified != release.manifest || verified["catalog_id"] != expected {
        return Err(CompassError::IntegrityViolation("local release differs from its committed manifest".into()));
    }
    if release.version > 1 {
        changes::verify_publication(ownership::verified_provider(family)?, dataset, release)?;
    }
    let engine = DuckDbCatalogEngine::open_hospital_readonly(path.join("ducklake"))?;
    Ok((engine, verified))
}

fn open_release(store: &LocalStore, dataset: &str) -> CompassResult<Option<(DuckDbCatalogEngine, Value)>> {
    // Retrying the original import always resolves the original receipt, even after an append.
    open_version(store, dataset, Some(1))
}

pub(crate) async fn restore(root: &Path, runtime: &InProcessNodeRuntime) -> CompassResult<()> {
    let home = root.join(STORE);
    if !home.exists() { return Ok(()); }
    let store = LocalStore::open(&home).map_err(store_error)?;
    for entry in fs::read_dir(home).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        if !entry.file_type().map_err(storage)?.is_dir() { continue; }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let family = match store.published_state(name) {
            Ok(s) => s,
            Err(e) => { tracing::warn!(dataset=name, error=%e, "skipping unreadable private import"); continue; }
        };
        for release in family.history.iter().chain(&family.release) {
            match open_committed(&store, name, &family, release) {
                Ok((engine, _)) => runtime.register_immutable_catalog(engine).await?,
                Err(e) => tracing::warn!(dataset=name, version=release.version, error=%e, "skipping unverified private import"),
            }
        }
    }
    Ok(())
}

fn receipt(
    engine: &DuckDbCatalogEngine,
    release: &Value,
    source: &str,
    pin: &Value,
) -> CompassResult<Value> {
    if release["source_sha256"] != source || release["validation"]["importer"] != *pin {
        return Err(CompassError::IntegrityViolation(
            "dataset name already belongs to another source or importer version".into(),
        ));
    }
    Ok(json!({"source_sha256":source,"report":release["validation"],"catalog":engine.summary()}))
}

fn pin(module: &Installed) -> CompassResult<Value> {
    let manifest = serde_json::to_value(&module.manifest).map_err(storage)?;
    Ok(json!({"plugin_id":module.manifest.id,"version":module.manifest.version,
        "image":module.image.raw,"manifest_sha256":hex::encode(Sha256::digest(manifest.to_string().as_bytes()))}))
}

async fn execute(state: &AppState, module: &Installed, inbound: &Path, outbound: &Path) -> CompassResult<()> {
    let runner = state
        .oci_runner
        .as_ref()
        .ok_or_else(|| CompassError::ContainerEngine("local engine unavailable".into()))?;
    let mut resources = module.manifest.resources.clone();
    resources.cpus = resources.cpus.clamp(1, 2);
    resources.memory_mb = resources.memory_mb.clamp(1, 2048);
    resources.timeout_sec = resources.timeout_sec.clamp(1, 900);
    resources.pids_limit = resources.pids_limit.clamp(1, 128);
    resources.gpu = 0;
    let execution = runner
        .execute(
            &ContainerRunSpec {
                run_id: RunId::new(format!("import-{}", uuid::Uuid::new_v4()))
                    .map_err(CompassError::Validation)?,
                image: module.image.clone(),
                in_dir: inbound.to_path_buf(),
                out_dir: outbound.to_path_buf(),
                resources,
                network: NetworkPolicy::default(),
                command_override: None,
            },
            None,
        )
        .await?;
    if execution.exit_code != 0 {
        return Err(
            CompassError::Validation(format!("importer exited {}", execution.exit_code)),
        );
    }
    let output = outbound.to_path_buf();
    tokio::task::spawn_blocking(move || check_tree(&output)).await.map_err(storage)?
}

pub(crate) async fn import(
    State(state): State<AppState>,
    AxumPath((ns, slug)): AxumPath<(String, String)>,
    Query(query): Query<ImportQuery>,
    request: Request,
) -> ApiResult<Json<Value>> {
    operator::authorize(&state, request.headers())?;
    let runtime = local(&state)?;
    let id = PluginId::new(format!("{ns}/{slug}")).map_err(CompassError::Validation)?;
    let module = installed(&state, &id, &query.version).await?;
    if query.filename.is_empty()
        || query.filename.contains(['/', '\\'])
        || query.filename.chars().any(char::is_control)
        || !module.contract.accepts(&query.filename)
    {
        return Err(CompassError::Validation(
            "filename does not match the selected importer's declared format".into(),
        )
        .into());
    }
    let store = LocalStore::open(&state.data_dir.join(STORE)).map_err(store_error)?;
    store
        .dataset_directory(&query.dataset_name)
        .map_err(store_error)?;
    if query.for_append {
        changes::authorize_append(&state, &store, &query.dataset_name, &module).await?;
    }
    let catalog_id = CatalogId::new(format!("bc-private-v1/{}", query.dataset_name))
        .map_err(CompassError::Validation)?;
    let stage = tempfile::Builder::new()
        .prefix(".native-import-")
        .tempdir_in(&state.data_dir)
        .map_err(storage)?;
    let inbound = stage.path().join("in");
    let outbound = stage.path().join("out");
    prepare_stage_permissions(stage.path(), &inbound, &outbound)?;
    let data = inbound.join("data");
    fs::create_dir(&data).map_err(storage)?;
    let source = data.join("bundle.zip");
    let mut input = tokio::fs::File::create(&source).await.map_err(storage)?;
    let mut bytes = 0u64;
    let mut hash = Sha256::new();
    let mut chunks = request.into_body().into_data_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(storage)?;
        bytes += chunk.len() as u64;
        if bytes > MAX_BYTES {
            return Err(CompassError::Validation("maximum import size is 32 GiB".into()).into());
        }
        input.write_all(&chunk).await.map_err(storage)?;
        hash.update(&chunk);
    }
    if bytes == 0 {
        return Err(CompassError::Validation("uploaded file is empty".into()).into());
    }
    input.flush().await.map_err(storage)?;
    drop(input);
    let source_sha = hex::encode(hash.finalize());
    let image = module.image.raw.clone();
    let pin = pin(&module)?;
    if !query.for_append {
    let previous_store = store.clone();
    let previous_name = query.dataset_name.clone();
    let existing =
        tokio::task::spawn_blocking(move || open_release(&previous_store, &previous_name))
            .await
            .map_err(storage)??;
    if let Some((engine, release)) = existing {
        owner(&state, &store, &query.dataset_name)?;
        let response = receipt(&engine, &release, &source_sha, &pin)?;
        runtime.register_immutable_catalog(engine).await?;
        return Ok(Json(response));
    }
    }
    fs::write(
        inbound.join("params.json"),
        b"{\"operation\":\"import\",\"bundle_filename\":\"bundle.zip\"}",
    )
    .map_err(storage)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&source, &inbound.join("params.json")] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o644)).map_err(storage)?;
        }
        fs::set_permissions(&data, fs::Permissions::from_mode(0o755)).map_err(storage)?;
    }
    execute(&state, &module, &inbound, &outbound).await?;
    let check_dir = outbound.clone();
    let mut report = tokio::task::spawn_blocking(move || read_report(&check_dir.join("result.json")))
        .await
        .map_err(storage)??;
    if report["source_sha256"] != source_sha || !report["valid"].is_boolean() {
        return Err(CompassError::IntegrityViolation(
            "importer report does not match the uploaded bytes".into(),
        )
        .into());
    }
    report["original_sha256"] = json!(&source_sha);
    report["importer"] = pin.clone();
    if report["valid"] != true {
        let draft = state
            .data_dir
            .join("import-drafts")
            .join(hex::encode(Sha256::digest(pin.to_string())))
            .join(&source_sha);
        let mut dir = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            dir.mode(0o700);
        }
        dir.recursive(true).create(&draft).map_err(storage)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&draft, fs::Permissions::from_mode(0o700)).map_err(storage)?;
        }
        match fs::hard_link(&source, draft.join("source")) {
            Ok(()) => (),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(err) => return Err(storage(err).into()),
        }
        let temporary = tempfile::NamedTempFile::new_in(&draft).map_err(storage)?;
        serde_json::to_writer(temporary.as_file(), &report).map_err(storage)?;
        temporary.as_file().sync_all().map_err(storage)?;
        match temporary.persist_noclobber(draft.join("report.json")) {
            Ok(_) => (),
            Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(err) => return Err(storage(err).into()),
        }
        return Ok(Json(
            json!({"source_sha256":source_sha,"report":report,"catalog":null}),
        ));
    }
    if query.for_append {
        let sha = source_sha.clone();
        let choice = tokio::task::spawn_blocking(move || {
            let _stage = stage;
            changes::save_append(&state, &store, &query.dataset_name, &sha, &pin, &source,
                                &outbound.join("rendered/bundle.zip"), &query.filename)
        }).await.map_err(storage)??;
        return Ok(Json(json!({"source_sha256":source_sha,"source":choice,"report":report,"catalog":null})));
    }
    let org = state.default_org_id.to_string();
    let actor = runtime.provider_id();
    let signing_runtime = runtime.clone();
    let dataset = query.dataset_name;
    let (engine, result) = tokio::task::spawn_blocking(move || -> CompassResult<_> {
        let _stage = stage;
        let draft_id = format!("sha256-{source_sha}");
        match store.provision_validated(
            &dataset,
            &org,
            &draft_id,
            &source_sha,
            &image,
            vec![actor.clone()],
            vec![actor.clone()],
            report.clone(),
        ) {
            Ok(()) => (),
            Err(e) if e.starts_with("conflict:") => (),
            Err(e) => return Err(store_error(e)),
        }
        let saved = store
            .read_state(&dataset, &org, &actor)
            .map_err(store_error)?;
        if saved.source_sha != source_sha
            || saved.validation.as_ref().and_then(|v| v.get("importer")) != Some(&pin)
        {
            return Err(CompassError::IntegrityViolation(
                "dataset name already belongs to another source or importer version".into(),
            ));
        }
        store
            .publish(&dataset, &org, &actor, saved.revision, |dir, _| {
                private_release::seal_version(
                    dir,
                    &outbound.join("rendered/tables"),
                    catalog_id.as_str(),
                    &report,
                    &source,
                    json!([]),
                    1,
                    None,
                    &|| {},
                )
            })
            .map_err(store_error)?;
        ownership::bind(&signing_runtime, &store, &dataset, &org)?;
        let (engine, release) =
            open_release(&store, &dataset)?.ok_or_else(|| storage("missing committed release"))?;
        let result = receipt(&engine, &release, &source_sha, &pin)?;
        Ok((engine, result))
    })
    .await
    .map_err(storage)??;
    runtime.register_immutable_catalog(engine).await?;
    Ok(Json(result))
}
