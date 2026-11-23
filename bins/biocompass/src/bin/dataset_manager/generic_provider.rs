//! Publish an existing sealed tabular catalog with an approved query relation.
use super::store::{LocalStore, State};
use biocompass_core_traits::CatalogId;
use biocompass_data_engine::{DuckLakeCatalogEngine, ShardCache, scoped_join::ScopeRelation};
use serde_json::{Value, json};
use std::{fs, io::{ErrorKind, Write}, path::Path};

pub fn admit(
    store: &LocalStore,
    dataset: &str,
    org: &str,
    publishers: Vec<String>,
    source: &Path,
    relation_path: &Path,
) -> Result<State, String> {
    let invalid = |error| format!("invalid: {error}");
    let relation_file = fs::File::open(relation_path).map_err(|error| format!("storage: {error}"))?;
    let relation: ScopeRelation =
        serde_json::from_reader(relation_file).map_err(|error| format!("invalid: {error}"))?;
    let catalog = DuckLakeCatalogEngine::open_verified(source).map_err(invalid)?;
    let manifest = build_manifest(&catalog, &relation)?;
    let descriptor = with_visibility(&manifest);
    let state = store.admit_sealed(dataset, org, publishers, manifest, |object| {
        ensure_descriptor(object, &descriptor)?;
        let destination = object.join("ducklake");
        fs::create_dir(&destination).map_err(|error| format!("storage: {error}"))?;
        copy_selected(&catalog, &destination)?;
        let verified = DuckLakeCatalogEngine::open_verified(&destination).map_err(invalid)?;
        if verified.summary().latest_sealed != catalog.summary().latest_sealed {
            return Err("conflict: copied catalog seal differs from original".into());
        }
        Ok(())
    })?;
    // Idempotent re-admission also makes an already verified native release portable.
    let release = state.release.as_ref().ok_or_else(|| "storage: missing admitted release".to_string())?;
    let object = store.dataset_directory(dataset)?.join(&release.object_dir);
    ensure_descriptor(&object, &descriptor)?;
    Ok(state)
}

pub fn verify(object: &Path, pinned: &Value, relation: &ScopeRelation) -> Result<Value, String> {
    let catalog = DuckLakeCatalogEngine::open_verified(object.join("ducklake"))
        .map_err(|error| format!("storage: {error}"))?;
    let manifest = build_manifest(&catalog, relation)?;
    if &manifest != pinned {
        return Err("conflict: persisted catalog differs from committed version".into());
    }
    if descriptor_matches(object, &with_visibility(&manifest))? == Some(false) {
        return Err("conflict: persisted catalog differs from committed version".into());
    }
    Ok(manifest)
}

fn with_visibility(manifest: &Value) -> Value {
    let mut descriptor = manifest.clone();
    descriptor["visibility"] = json!("private");
    descriptor
}

fn descriptor_matches(object: &Path, descriptor: &Value) -> Result<Option<bool>, String> {
    match fs::read(object.join("release.json")) {
        Ok(raw) => {
            let existing: Value = serde_json::from_slice(&raw).map_err(|error| format!("storage: {error}"))?;
            Ok(Some(&existing == descriptor))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("storage: {error}")),
    }
}

fn ensure_descriptor(object: &Path, descriptor: &Value) -> Result<(), String> {
    let storage = |error| format!("storage: write release descriptor: {error}");
    if descriptor_matches(object, descriptor)?.is_none() {
        let mut draft = tempfile::NamedTempFile::new_in(object).map_err(storage)?;
        serde_json::to_writer(&mut draft, descriptor).map_err(|error| format!("storage: {error}"))?;
        draft.flush().map_err(storage)?;
        draft.as_file().sync_all().map_err(storage)?;
        if let Err(error) = draft.persist_noclobber(object.join("release.json"))
            && error.error.kind() != ErrorKind::AlreadyExists
        {
            return Err(storage(error.error));
        }
        fs::File::open(object).and_then(|directory| directory.sync_all()).map_err(storage)?;
    }
    if descriptor_matches(object, descriptor)? != Some(true) {
        return Err("conflict: portable descriptor differs from committed version".into());
    }
    Ok(())
}

fn build_manifest(catalog: &DuckLakeCatalogEngine, relation: &ScopeRelation) -> Result<Value, String> {
    let invalid = |error| format!("invalid: {error}");
    let summary = catalog.summary();
    CatalogId::ensure_legacy_writable(&summary.id).map_err(invalid)?;
    if summary.id != summary.latest_sealed.catalog_id
        || summary.latest_snapshot_id != summary.latest_sealed.snapshot_id
    {
        return Err("conflict: catalog identity and latest snapshot disagree".into());
    }
    relation.validate(&summary.latest_sealed).map_err(invalid)?;
    Ok(json!({
        "catalog_id": summary.id,
        "catalog_version": summary.version,
        "snapshot_id": summary.latest_snapshot_id,
        "dataset_digest": summary.dataset_digest,
        "scope_relation": relation,
    }))
}

fn copy_selected(engine: &DuckLakeCatalogEngine, destination: &Path) -> Result<(), String> {
    let storage = |error| format!("storage: copy sealed catalog: {error}");
    let encode = |error| format!("storage: encode sealed catalog: {error}");
    let integrity = |error| format!("conflict: copy sealed catalog: {error}");
    let mut summary = engine.summary().clone();
    summary.snapshots.retain(|record| record.snapshot_id == summary.latest_snapshot_id);
    let snapshot = &summary.latest_sealed;
    let snapshot_dir = destination.join("snapshots");
    fs::create_dir(&snapshot_dir).map_err(storage)?;
    fs::write(destination.join("catalog.json"), serde_json::to_vec(&summary).map_err(encode)?)
        .map_err(storage)?;
    fs::write(snapshot_dir.join(format!("snapshot_{}.json", snapshot.snapshot_id)),
        serde_json::to_vec(snapshot).map_err(encode)?).map_err(storage)?;
    let cache = ShardCache::open(destination).map_err(integrity)?;
    for digest in snapshot.referenced_digests() {
        let source = engine.shard_cache().shard_path(&digest).map_err(integrity)?;
        if !source.symlink_metadata().map_err(storage)?.is_file() {
            return Err("invalid: sealed shard must be a regular file".into());
        }
        fs::copy(source, cache.shard_path(&digest).map_err(integrity)?).map_err(storage)?;
    }
    Ok(())
}
