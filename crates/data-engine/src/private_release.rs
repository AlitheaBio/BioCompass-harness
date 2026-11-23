//! Assemble and reopen the immutable release shared by native and hosted publishers.

use crate::{
    DuckLakeCatalogEngine, IngestMode, apply_normalized_parquet_to_ducklake,
    open_duckdb_with_extensions,
};
use biocompass_core_traits::{CatalogId, CompassError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

fn storage(e: impl std::fmt::Display) -> String {
    format!("storage: {e}")
}

fn sha256(path: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();
    io::copy(&mut File::open(path).map_err(storage)?, &mut hasher).map_err(storage)?;
    Ok(hex::encode(hasher.finalize()))
}

fn string_field<'a>(value: &'a Value, name: &str) -> Result<&'a str, String> {
    value[name]
        .as_str()
        .ok_or_else(|| format!("invalid: expected string field {name}"))
}

fn archive(source: &Path, target: &Path, digest: &str) -> Result<(), String> {
    let mut file = File::create_new(target).map_err(storage)?;
    io::copy(&mut File::open(source).map_err(storage)?, &mut file).map_err(storage)?;
    file.sync_all().map_err(storage)?;
    if sha256(target)? != digest {
        return Err(format!(
            "invalid: archive checksum mismatch for {}",
            target.display()
        ));
    }
    Ok(())
}

fn legal_table(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == b'_')
        && !name.as_bytes()[0].is_ascii_digit()
}

#[allow(clippy::too_many_arguments)]
pub fn seal_version(
    object_dir: &Path,
    tables: &Path,
    catalog_id: &str,
    validation: &Value,
    source: &Path,
    audit: Value,
    version: u64,
    change: Option<Value>,
    on_first_table: &dyn Fn(),
) -> Result<Value, String> {
    if validation["valid"] != true {
        return Err("invalid: cannot seal data without a successful validation".into());
    }
    let original_hash = string_field(validation, "original_sha256")?;
    let derived_hash = string_field(validation, "source_sha256")?;
    let resources = validation["resources"]
        .as_array()
        .ok_or("invalid: no resource inventory")?;
    let mut hashes = BTreeMap::new();
    for resource in resources.iter().filter(|r| r["disposition"] == "table") {
        let name = string_field(resource, "table")?;
        if !legal_table(name) {
            return Err("invalid: unsupported table name".into());
        }
        let hash = string_field(resource, "sha256")?;
        if hashes.insert(name.to_owned(), hash.to_owned()).is_some() {
            return Err(format!("invalid: duplicate validated table {name}"));
        }
    }
    if hashes.is_empty() {
        return Err("invalid: no tables to seal".into());
    }
    let inputs = tables.parent().ok_or("invalid: missing table parent")?;
    let stable_dir = object_dir.canonicalize().map_err(storage)?;
    let catalog_dir = stable_dir.join("ducklake");
    if catalog_dir.exists() {
        return Err("conflict: immutable release directory already contains a catalog".into());
    }
    let mut found = 0;
    for entry in fs::read_dir(tables).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        let path = entry.path();
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("invalid: invalid table filename")?;
        if path.extension().and_then(|s| s.to_str()) != Some("parquet")
            || !entry.file_type().map_err(storage)?.is_file()
            || hashes.get(name) != Some(&sha256(&path)?)
        {
            return Err(format!(
                "invalid: table {name} differs from the validator's inputs"
            ));
        }
        found += 1;
    }
    if found != hashes.len() {
        return Err("invalid: one or more validated tables are absent".into());
    }
    let index_file = File::open(inputs.join("resources.json")).map_err(storage)?;
    let resource_index: Value = serde_json::from_reader(index_file).map_err(storage)?;
    let keys: BTreeMap<String, Vec<String>> =
        serde_json::from_value(resource_index["primary_keys"].clone())
            .map_err(|e| format!("invalid: primary keys in resource inventory: {e}"))?;
    for (name, key) in &keys {
        if !hashes.contains_key(name) || key.iter().any(|field| !legal_table(field)) {
            return Err(format!("invalid: unrecognized key declaration for {name}"));
        }
    }
    archive(source, &stable_dir.join("source.zip"), original_hash)?;
    archive(
        &inputs.join("bundle.zip"),
        &stable_dir.join("validated.zip"),
        derived_hash,
    )?;
    let attachments = seal_attachments(&stable_dir, inputs, resources, &resource_index)?;
    let id = CatalogId::new(catalog_id)?;
    let engine = DuckLakeCatalogEngine::create_local_catalog(
        &catalog_dir,
        id,
        catalog_id,
        version.to_string(),
        "Private dataset version",
        "",
        keys.clone(),
        "Publish validated dataset",
        |conn| {
            conn.execute_batch("BEGIN TRANSACTION")
                .map_err(|e| CompassError::Storage(e.to_string()))?;
            for (i, name) in hashes.keys().enumerate() {
                apply_normalized_parquet_to_ducklake(
                    conn,
                    "dl",
                    &tables.join(format!("{name}.parquet")),
                    name,
                    IngestMode::Replace,
                    keys.get(name).map(Vec::as_slice).unwrap_or_default(),
                )?;
                if i == 0 {
                    on_first_table();
                }
            }
            conn.execute_batch("COMMIT")
                .map_err(|e| CompassError::Storage(e.to_string()))
        },
    )
    .map_err(storage)?;
    let snapshot = &engine.summary().latest_sealed;
    let mut table_index = BTreeMap::new();
    for table in &snapshot.tables {
        let schema = &table.schema;
        let hash = hashes
            .get(&schema.name)
            .ok_or("storage: unexpected table in sealed snapshot")?;
        table_index.insert(&schema.name, json!({
            "input_sha256": hash, "rows": schema.row_count, "schema": schema, "files": table.files
        }));
    }
    if hashes.len() != table_index.len() {
        return Err("storage: sealed snapshot inventory differs from validated inputs".into());
    }
    let mut manifest = json!({
        "format":"biocompass.private-dataset.v1", "visibility":"private", "catalog_id":catalog_id,
        "snapshot_id":snapshot.snapshot_id, "dataset_digest":snapshot.dataset_digest,
        "row_count":snapshot.total_row_count(), "table_count":snapshot.tables.len(), "tables":table_index,
        "source_sha256":original_hash, "validated_sha256":derived_hash,
        "resource_index":resource_index, "validation":validation, "audit":audit
    });
    if let Some(attachments) = attachments {
        manifest["attachments"] = Value::Array(attachments);
        manifest["raw_files"] = validation
            .get("raw_files")
            .cloned()
            .unwrap_or_else(|| json!([]));
    }
    if let Some(change) = change {
        manifest["version_change"] = change;
    }
    let mut manifest_file = File::create_new(stable_dir.join("release.json")).map_err(storage)?;
    serde_json::to_writer_pretty(&mut manifest_file, &manifest).map_err(storage)?;
    manifest_file.write_all(b"\n").map_err(storage)?;
    manifest_file.sync_all().map_err(storage)?;
    sync_tree(&catalog_dir)?;
    File::open(&stable_dir)
        .and_then(|f| f.sync_all())
        .map_err(storage)?;
    verify(&stable_dir)
}

fn attachment_digest(value: &Value) -> Result<&str, String> {
    let digest = string_field(value, "sha256")?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("invalid: attachment has an invalid SHA-256 digest".into());
    }
    Ok(digest)
}

fn check_attachment_size(path: &Path, item: &Value) -> Result<(), String> {
    let expected = item["bytes"]
        .as_u64()
        .ok_or("invalid: attachment requires a byte count")?;
    if fs::symlink_metadata(path).map_err(storage)?.is_file()
        && fs::metadata(path).map_err(storage)?.len() == expected
    {
        return Ok(());
    }
    Err("storage: immutable attachment length differs from the validated inventory".into())
}

fn seal_attachments(
    release: &Path,
    inputs: &Path,
    resources: &[Value],
    resource_index: &Value,
) -> Result<Option<Vec<Value>>, String> {
    match resource_index["attachment_layout"].as_str() {
        None if resource_index.get("attachment_layout").is_none() => return Ok(None),
        Some("sha256.v1") => {}
        _ => return Err("invalid: unsupported staged attachment layout".into()),
    }
    let destination = release.join("attachments");
    fs::create_dir(&destination).map_err(storage)?;
    let mut objects = Vec::new();
    let mut copied = std::collections::BTreeSet::new();
    for item in resources
        .iter()
        .filter(|entry| entry["disposition"] == "attachment")
    {
        let digest = attachment_digest(item)?;
        let path = string_field(item, "path")?;
        let staged = inputs.join("attachments").join(digest);
        check_attachment_size(&staged, item)?;
        if copied.insert(digest) {
            archive(&staged, &destination.join(digest), digest)?;
        }
        objects.push(json!({"path":path,"sha256":digest,"bytes":item["bytes"]}));
    }
    File::open(&destination)
        .and_then(|dir| dir.sync_all())
        .map_err(storage)?;
    Ok(Some(objects))
}

fn verify_attachments(object_dir: &Path, manifest: &Value) -> Result<(), String> {
    if let Some(attachments) = manifest.get("attachments") {
        let items = attachments
            .as_array()
            .ok_or("invalid: attachment inventory is not an array")?;
        for item in items {
            let digest = attachment_digest(item)?;
            let path = object_dir.join("attachments").join(digest);
            check_attachment_size(&path, item)?;
            if sha256(&path)? != digest {
                return Err(
                    "storage: immutable attachment SHA-256 differs from the validated inventory"
                        .into(),
                );
            }
        }
    }
    Ok(())
}

fn sync_tree(dir: &Path) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        if entry.file_type().map_err(storage)?.is_dir() {
            sync_tree(&entry.path())?;
        } else {
            File::open(entry.path())
                .and_then(|f| f.sync_all())
                .map_err(storage)?;
        }
    }
    File::open(dir).and_then(|f| f.sync_all()).map_err(storage)
}

/// Verify committed file hashes, sealed shard bytes and pinned DuckLake row counts.
/// Compare the returned object to the independently committed head's manifest.
pub fn verify(object_dir: &Path) -> Result<Value, String> {
    let file = File::open(object_dir.join("release.json")).map_err(storage)?;
    let manifest: Value = serde_json::from_reader(file).map_err(storage)?;
    if let Some(expected) = manifest["version_change"]["append_sha256"].as_str()
        && sha256(&object_dir.join("append.zip"))? != expected
    {
        return Err("storage: appended archive failed release integrity verification".into());
    }
    for (file, field) in [
        ("source.zip", "source_sha256"),
        ("validated.zip", "validated_sha256"),
    ] {
        if sha256(&object_dir.join(file))? != string_field(&manifest, field)? {
            return Err(format!(
                "storage: release archive integrity failure for {file}"
            ));
        }
    }
    verify_attachments(object_dir, &manifest)?;
    let dir = object_dir.join("ducklake");
    let engine = DuckLakeCatalogEngine::open_verified(&dir).map_err(storage)?;
    let snapshot = &engine.summary().latest_sealed;
    if manifest["dataset_digest"] != snapshot.dataset_digest
        || manifest["snapshot_id"] != snapshot.snapshot_id
        || manifest["catalog_id"].as_str() != Some(snapshot.catalog_id.as_str())
        || manifest["row_count"] != snapshot.total_row_count()
        || manifest["table_count"].as_u64() != Some(snapshot.tables.len() as u64)
    {
        return Err("storage: release manifest and sealed snapshot disagree".into());
    }
    let conn = open_duckdb_with_extensions(&["ducklake"]).map_err(storage)?;
    let meta = dir.join("meta.ducklake");
    let quoted = meta
        .to_str()
        .ok_or("storage: invalid data path")?
        .replace('\'', "''");
    conn.execute_batch(&format!(
        "ATTACH 'ducklake:{quoted}' AS released (READ_ONLY)"
    ))
    .map_err(storage)?;
    for table in &snapshot.tables {
        let name = &table.schema.name;
        if !legal_table(name) {
            return Err("storage: invalid sealed table identifier".into());
        }
        let rows: u64 = conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM released.\"{name}\" AT (VERSION => {})",
                    snapshot.snapshot_id
                ),
                [],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if rows != table.schema.row_count || manifest["tables"][name]["rows"] != rows {
            return Err(format!("storage: pinned row count mismatch for {name}"));
        }
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn seal(
        object_dir: &Path,
        tables: &Path,
        catalog_id: &str,
        validation: &Value,
        source: &Path,
        audit: Value,
    ) -> Result<Value, String> {
        seal_version(
            object_dir,
            tables,
            catalog_id,
            validation,
            source,
            audit,
            1,
            None,
            &|| {},
        )
    }

    #[test]
    fn invalid_validation_cannot_create_a_catalog() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            seal(
                root.path(),
                root.path(),
                "tenant/test",
                &serde_json::json!({"valid": false}),
                &root.path().join("no-source"),
                serde_json::json!([])
            )
            .is_err()
        );
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn reopen_two_tables_and_check_checked_input_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let tables = dir.join("tables");
        let obj = dir.join("object");
        fs::create_dir(&tables).unwrap();
        fs::create_dir(&obj).unwrap();
        let conn = open_duckdb_with_extensions(&[]).unwrap();
        let mut resources = Vec::new();
        for name in ["instruments", "readings"] {
            let path = tables.join(format!("{name}.parquet"));
            conn.execute_batch(&format!(
                "COPY (SELECT 1 AS id) TO '{}' (FORMAT PARQUET)",
                path.to_str().unwrap().replace('\'', "''")
            ))
            .unwrap();
            resources
                .push(json!({"disposition":"table","table":name,"sha256":sha256(&path).unwrap()}));
        }
        let archive = dir.join("bundle.zip");
        fs::write(&archive, b"validator-approved fixture archive").unwrap();
        fs::write(dir.join("resources.json"), b"{\"primary_keys\":{}}").unwrap();
        let digest = sha256(&archive).unwrap();
        let validation = json!({
            "valid":true,"resources":resources,"source_sha256":digest,"original_sha256":digest
        });
        let release = seal(
            &obj,
            &tables,
            "test/instrument-readings",
            &validation,
            &archive,
            json!([]),
        )
        .unwrap();
        assert_eq!(release["table_count"], 2);
        assert_eq!(release["row_count"], 2);
        assert_eq!(release, verify(&obj).unwrap());
        fs::write(obj.join("source.zip"), b"altered").unwrap();
        assert!(verify(&obj).unwrap_err().contains("integrity failure"));
    }
}
