use serde_json::Value;
use std::path::Path;

pub use biocompass_data_engine::private_release::verify;

pub fn seal(
    object_dir: &Path,
    tables: &Path,
    catalog_id: &str,
    validation: &Value,
    source: &Path,
    audit: Value,
) -> Result<Value, String> {
    seal_version(
        object_dir, tables, catalog_id, validation, source, audit, 1, None,
    )
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
) -> Result<Value, String> {
    biocompass_data_engine::private_release::seal_version(
        object_dir,
        tables,
        catalog_id,
        validation,
        source,
        audit,
        version,
        change,
        &|| super::checkpoint("first_table_staged"),
    )
}
