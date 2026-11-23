//! `biocompass-data-engine`: DuckLake + Apache Arrow `RecordBatch` catalog engine with:
//! - Offline signed DuckDB `v2.0.0` extension loading (`allow_unsigned_extensions = false`)
//! - Schema-generic multi-table `CatalogSummary` + non-sensitive `facets`
//! - Typed, SQL-free `QueryAst` & recursive `?`-bound `FilterExpr`
//! - Archive-on-seal SHA-256 `SealedSnapshot` (`dataset_digest`) & verify-once `ShardCache` (`B12`)
//! - Same-plane multi-catalog attach at pinned snapshots
//! - Snapshot pinning, selective expiry, forward-commit `rollback`, and `READ_ONLY` hospital attach
//! - 3-level `DatasetGrant` (`compute_only` vs `rows`, zero `***` masking) & fail-loud full-scan staging (`B11`)

pub mod private_release;
pub mod scoped_join;
mod shard_cache;
pub mod seed;

pub use biocompass_core_traits::{
    AttachedCatalogPin, ColumnDescriptor, FilterExpr, GrantLevel, IngestFormat, IngestMode,
    QueryAst, ScalarValue, SortDirection, SortSpec, TableSchema,
};
pub use duckdb::arrow::record_batch::RecordBatch;
pub use shard_cache::ShardCache;
pub use seed::{seed_demo_hla_catalog, seed_local_hla_catalog_with_id};

use biocompass_core_traits::{CatalogId, CompassError, CompassResult, DatasetGrant};
use chrono::Utc;
use duckdb::arrow::array::AsArray;
use duckdb::arrow::array::{
    Array, BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
    LargeStringArray, StringArray, StringViewArray, UInt8Array, UInt16Array, UInt32Array,
    UInt64Array,
};
use duckdb::arrow::datatypes::DataType;
use duckdb::arrow::util::display::{ArrayFormatter, FormatOptions, array_value_to_string};
use duckdb::types::Value as DuckValue;
use duckdb::{Config, Connection, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Type aliases preserving clean ergonomics across workspace crates.
pub type CatalogManifest = CatalogSummary;
pub type CatalogQuery = QueryAst;
pub type CatalogQueryResult = QueryTableResult;
pub type DuckDbCatalogEngine = DuckLakeCatalogEngine;

/// Default facet columns automatically published when present in a table and non-sensitive.
const DEFAULT_FACET_COLUMNS: &[&str] = &["hla_allele", "organ", "condition"];

/// Content-addressed data + optional positional-delete `(file_path, pos)` Parquet pair in a sealed snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedFilePair {
    pub data_sha256: String,
    pub data_size_bytes: u64,
    #[serde(default)]
    pub delete_sha256: Option<String>,
    #[serde(default)]
    pub delete_size_bytes: Option<u64>,
}

/// Per-table schema and immutable content-addressed shard list inside a `SealedSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedTableSnapshot {
    pub schema: TableSchema,
    pub files: Vec<SealedFilePair>,
}

/// Cryptographically sealed catalog snapshot (`dataset_digest` binds every table schema & shard SHA-256).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedSnapshot {
    pub catalog_id: CatalogId,
    pub snapshot_id: u64,
    pub dataset_digest: String,
    pub created_at: String,
    pub commit_message: String,
    pub tables: Vec<SealedTableSnapshot>,
    #[serde(default)]
    pub facets: BTreeMap<String, Vec<String>>,
}

impl SealedSnapshot {
    /// Deterministic SHA-256 `dataset_digest` over canonical table schemas and `(data_sha256, delete_sha256)` pairs.
    pub fn compute_dataset_digest(
        catalog_id: &CatalogId,
        snapshot_id: u64,
        tables: &[SealedTableSnapshot],
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"biocompass-sealed-snapshot:v2:");
        hasher.update(catalog_id.as_str().as_bytes());
        hasher.update(b":");
        hasher.update(snapshot_id.to_string().as_bytes());
        for tbl in tables {
            hasher.update(b"|table:");
            hasher.update(tbl.schema.name.as_bytes());
            for col in &tbl.schema.columns {
                hasher.update(b",col:");
                hasher.update(col.name.as_bytes());
                hasher.update(b":");
                hasher.update(col.data_type.as_bytes());
            }
            for pk in &tbl.schema.primary_key {
                hasher.update(b",pk:");
                hasher.update(pk.as_bytes());
            }
            let mut sorted_files = tbl.files.clone();
            sorted_files.sort_by(|a, b| {
                a.data_sha256
                    .cmp(&b.data_sha256)
                    .then_with(|| a.delete_sha256.cmp(&b.delete_sha256))
            });
            for fp in &sorted_files {
                hasher.update(b",data:");
                hasher.update(fp.data_sha256.as_bytes());
                if let Some(del) = &fp.delete_sha256 {
                    hasher.update(b"+del:");
                    hasher.update(del.as_bytes());
                }
            }
        }
        hex::encode(hasher.finalize())
    }

    pub fn total_row_count(&self) -> u64 {
        self.tables.iter().map(|t| t.schema.row_count).sum()
    }

    /// Resolve a target table (`None` selects the primary/first table).
    pub fn resolve_table(&self, table_name: Option<&str>) -> CompassResult<&SealedTableSnapshot> {
        match table_name.map(str::trim).filter(|s| !s.is_empty()) {
            Some(requested) => self
                .tables
                .iter()
                .find(|t| t.schema.name == requested)
                .ok_or_else(|| {
                    CompassError::NotFound(format!(
                        "table '{requested}' not found in catalog '{}' snapshot {}",
                        self.catalog_id, self.snapshot_id
                    ))
                }),
            None => self.tables.first().ok_or_else(|| {
                CompassError::Validation(format!(
                    "catalog '{}' snapshot {} contains no tables",
                    self.catalog_id, self.snapshot_id
                ))
            }),
        }
    }

    /// Collect all unique content-addressed SHA-256 shard digests referenced by this snapshot.
    pub fn referenced_digests(&self) -> BTreeSet<String> {
        let mut set = BTreeSet::new();
        for tbl in &self.tables {
            for f in &tbl.files {
                set.insert(f.data_sha256.clone());
                if let Some(del) = &f.delete_sha256 {
                    set.insert(del.clone());
                }
            }
        }
        set
    }
}

/// Summary metadata for one historical sealed snapshot on a catalog card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotSummary {
    pub snapshot_id: u64,
    pub dataset_digest: String,
    pub row_count: u64,
    pub created_at: String,
    pub commit_message: String,
    #[serde(default)]
    pub pinned: bool,
}

/// Published catalog summary (`catalog.json`) carrying table schemas, facets, and sealed snapshot metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogSummary {
    pub id: CatalogId,
    pub name: String,
    pub version: String,
    pub description: String,
    pub organism: String,
    pub row_count: u64,
    pub latest_snapshot_id: u64,
    pub dataset_digest: String,
    pub tables: Vec<TableSchema>,
    /// Non-sensitive distinct values (e.g. `hla_alleles`, `organs`, `conditions`) published on the catalog card.
    #[serde(default)]
    pub facets: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub snapshots: Vec<SnapshotSummary>,
    pub latest_sealed: SealedSnapshot,
    /// Provider-governed relation attached to an admitted immutable tabular seal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_relation: Option<scoped_join::ScopeRelation>,
}

impl CatalogSummary {
    pub fn dataset_digest(&self) -> String {
        self.dataset_digest.clone()
    }

    /// Build the reader metadata for a verified immutable version without copying the old head's shard lists.
    pub fn with_pinned_seal(&self, seal: SealedSnapshot) -> CompassResult<Self> {
        let expected = if seal.snapshot_id == self.latest_snapshot_id {
            &self.dataset_digest
        } else {
            &self.snapshots.iter()
                .find(|entry| entry.snapshot_id == seal.snapshot_id)
                .ok_or_else(|| CompassError::NotFound(format!("unregistered snapshot {}", seal.snapshot_id)))?
                .dataset_digest
        };
        if seal.catalog_id != self.id || &seal.dataset_digest != expected {
            return Err(CompassError::IntegrityViolation("seal differs from registered snapshot".into()));
        }
        let row_count = seal.tables.iter().try_fold(0u64, |total, table| {
            total.checked_add(table.schema.row_count)
                .ok_or_else(|| CompassError::IntegrityViolation("sealed table row counts overflow".into()))
        })?;
        Ok(Self {
            id: self.id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            description: self.description.clone(),
            organism: self.organism.clone(),
            row_count,
            latest_snapshot_id: seal.snapshot_id,
            dataset_digest: seal.dataset_digest.clone(),
            tables: seal.tables.iter().map(|table| table.schema.clone()).collect(),
            facets: seal.facets.clone(),
            snapshots: self.snapshots.clone(),
            scope_relation: self.scope_relation.clone(),
            latest_sealed: seal,
        })
    }

}

/// API-edge JSON table payload serialized from Arrow `RecordBatch`es (§5.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryTableResult {
    pub catalog_id: CatalogId,
    pub snapshot_id: u64,
    pub dataset_digest: String,
    pub table: String,
    pub grant_id: String,
    pub access_level: GrantLevel,
    pub columns: Vec<ColumnDescriptor>,
    pub total_in_scope: usize,
    pub rows: Vec<Map<String, Value>>,
    #[serde(default)]
    pub remaining_daily_row_budget: Option<u64>,
}

/// Trust mode for container `/in/data/cohort.parquet` materialization (§2.1 / §5.3 / `B11`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StagingMode {
    /// Digest-approved curated module: allowed on `compute_only` and `rows`; stages full grant-scoped cohort.
    CuratedFullScan,
    /// Custom org module or local/unapproved module: refused on `compute_only`; fails loud if rows > `max_rows_per_query`.
    CustomerScoped,
}

/// Metadata for a Parquet slice materialized into `/in/data/cohort.parquet`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterializedSlice {
    pub path: PathBuf,
    pub row_count: u64,
    pub sha256: String,
    pub snapshot_id: u64,
    pub dataset_digest: String,
}

/// Canonical primary-key selection digest computed over the full grant-scoped query result (§7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionDigestResult {
    pub catalog_id: CatalogId,
    pub table_name: String,
    pub primary_key_columns: Vec<String>,
    pub snapshot_id: u64,
    pub dataset_digest: String,
    pub row_count: u64,
    pub selection_digest: String,
}

/// Input specification mounted at `/in/ingest_spec.json` inside Stage 1's airgapped validator container (§7.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuarantineIngestSpec {
    pub filename: String,
    pub format: IngestFormat,
    pub mode: IngestMode,
    pub table_name: String,
    #[serde(default)]
    pub primary_key: Vec<String>,
    #[serde(default)]
    pub expected_schema: Option<TableSchema>,
}

/// Validated Parquet normalization report written to `/out/ingest_report.json` by Stage 1 (§7.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuarantineValidationReport {
    pub table_name: String,
    pub format: IngestFormat,
    pub mode: IngestMode,
    pub row_count: u64,
    pub columns: Vec<ColumnDescriptor>,
    pub primary_key: Vec<String>,
    pub normalized_bytes: u64,
    pub normalized_sha256: String,
}

/// Cloud DuckLake Postgres + S3 connection parameters (§4.5 / §5.1).
#[derive(Debug, Clone)]
pub struct CloudDuckLakeConfig {
    pub postgres_dsn: String,
    pub metadata_schema: String,
    pub s3_endpoint: String,
    pub s3_region: String,
    pub s3_bucket: String,
    pub s3_access_key: String,
    pub s3_secret_key: String,
    pub s3_use_ssl: bool,
    pub data_prefix: String,
}

/// DuckLake + Arrow `RecordBatch` catalog engine supporting local `.ducklake`, hospital `READ_ONLY`,
/// and worker content-addressed sealed snapshot execution.
#[derive(Debug, Clone)]
pub struct DuckLakeCatalogEngine {
    catalog_dir: PathBuf,
    shard_cache: ShardCache,
    summary: CatalogSummary,
    primary_keys: BTreeMap<String, Vec<String>>,
    pinned_snapshots: BTreeSet<u64>,
    read_only: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CatalogLocalSidecar {
    #[serde(default)]
    primary_keys: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pinned_snapshots: BTreeSet<u64>,
}

impl DuckLakeCatalogEngine {
    /// Open a local DuckLake catalog directory (`catalog.json` + `sha256/`), verifying any uncached
    /// sealed shards once on initial admission into `ShardCache` (`B12`).
    pub fn open_verified(catalog_dir: impl AsRef<Path>) -> CompassResult<Self> {
        Self::open_internal(catalog_dir.as_ref(), false)
    }

    /// Open a local DuckLake catalog in strict `READ_ONLY` mode for `hospital-edge` nodes (§5.1 / §5.4).
    pub fn open_hospital_readonly(catalog_dir: impl AsRef<Path>) -> CompassResult<Self> {
        let dir = catalog_dir.as_ref();
        let meta_path = dir.join("meta.ducklake");
        if meta_path.is_file() {
            let conn = open_duckdb_with_extensions(&["ducklake"])?;
            let escaped = sql_quote_internal_path(&meta_path.to_string_lossy());
            conn.execute_batch(&format!("ATTACH 'ducklake:{escaped}' AS hosp_ro (READ_ONLY); DETACH hosp_ro;"))
                .map_err(|e| CompassError::Storage(format!("verify hospital READ_ONLY DuckLake attach: {e}")))?;
        }
        Self::open_internal(dir, true)
    }

    /// Construct a read-only engine over a shared `ShardCache` and an already-verified `CatalogSummary`
    /// (used by `cloud-pool` `froglet-node` workers after populating `ShardCache` from S3).
    pub fn from_cached_summary(summary: CatalogSummary, shard_cache: ShardCache) -> CompassResult<Self> {
        let dir = shard_cache
            .sha256_dir()
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
        Self::from_cached_summary_at(dir, summary, shard_cache)
    }

    /// Construct a read-only engine with an explicit per-catalog metadata directory (`snapshots/snapshot_<sid>.json`)
    /// while sharing a worker-wide content-addressed `ShardCache` (`<cache_root>/sha256/<64-hex>.parquet`).
    pub fn from_cached_summary_at(
        catalog_dir: PathBuf,
        summary: CatalogSummary,
        shard_cache: ShardCache,
    ) -> CompassResult<Self> {
        for table in &summary.latest_sealed.tables {
            shard_cache.check_table(&table.schema.name)?;
        }
        Self::cached_reader(catalog_dir, summary, shard_cache)
    }

    /// Read the complete immutable seal using only the named tables' verified local shards.
    pub fn from_cached_tables_at(
        catalog_dir: PathBuf,
        summary: CatalogSummary,
        mut shard_cache: ShardCache,
        tables: &BTreeSet<String>,
    ) -> CompassResult<Self> {
        shard_cache.restrict_to(tables, &summary.latest_sealed)?;
        Self::cached_reader(catalog_dir, summary, shard_cache)
    }

    fn cached_reader(
        catalog_dir: PathBuf,
        summary: CatalogSummary,
        shard_cache: ShardCache,
    ) -> CompassResult<Self> {
        let mut primary_keys = BTreeMap::new();
        for t in &summary.tables {
            if !t.primary_key.is_empty() {
                primary_keys.insert(t.name.clone(), t.primary_key.clone());
            }
        }
        let pinned_snapshots = summary
            .snapshots
            .iter()
            .filter(|s| s.pinned)
            .map(|s| s.snapshot_id)
            .collect();
        let reader = Self {
            catalog_dir,
            shard_cache,
            summary,
            primary_keys,
            pinned_snapshots,
            read_only: true,
        };
        reader.verify_integrity()?;
        Ok(reader)
    }

    fn open_internal(dir: &Path, read_only: bool) -> CompassResult<Self> {
        let manifest_path = dir.join("catalog.json");
        let raw = std::fs::read_to_string(&manifest_path).map_err(|e| {
            CompassError::NotFound(format!("read {}: {e}", manifest_path.display()))
        })?;
        let summary: CatalogSummary = serde_json::from_str(&raw)
            .map_err(|e| CompassError::Validation(format!("decode catalog.json: {e}")))?;

        let shard_cache = ShardCache::open(dir)?;

        let sidecar = load_sidecar(dir);
        let mut primary_keys = sidecar.primary_keys;
        for tbl in &summary.tables {
            if !tbl.primary_key.is_empty() {
                primary_keys
                    .entry(tbl.name.clone())
                    .or_insert_with(|| tbl.primary_key.clone());
            }
        }
        let mut pinned_snapshots = sidecar.pinned_snapshots;
        for s in &summary.snapshots {
            if s.pinned {
                pinned_snapshots.insert(s.snapshot_id);
            }
        }

        let engine = Self {
            catalog_dir: dir.to_path_buf(),
            shard_cache,
            summary,
            primary_keys,
            pinned_snapshots,
            read_only,
        };
        engine.verify_integrity()?;
        Ok(engine)
    }

    pub const fn manifest(&self) -> &CatalogSummary {
        &self.summary
    }

    pub const fn summary(&self) -> &CatalogSummary {
        &self.summary
    }

    pub const fn shard_cache(&self) -> &ShardCache {
        &self.shard_cache
    }

    pub const fn catalog_dir(&self) -> &PathBuf {
        &self.catalog_dir
    }

    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Verify the current sealed snapshot's `dataset_digest` and content-addressed Parquet shards (`B12`).
    pub fn verify_integrity(&self) -> CompassResult<()> {
        let snap = &self.summary.latest_sealed;
        let recomputed =
            SealedSnapshot::compute_dataset_digest(&snap.catalog_id, snap.snapshot_id, &snap.tables);
        if snap.catalog_id != self.summary.id
            || snap.snapshot_id != self.summary.latest_snapshot_id
            || recomputed != snap.dataset_digest
            || snap.dataset_digest != self.summary.dataset_digest
        {
            return Err(CompassError::IntegrityViolation(format!(
                "catalog '{}' sealed snapshot {} digest mismatch (expected {}, got {})",
                self.summary.id, snap.snapshot_id, snap.dataset_digest, recomputed
            )));
        }
        for table in &snap.tables {
            if self.shard_cache.selected_tables().is_some_and(|names| !names.contains(&table.schema.name)) {
                continue;
            }
            for pair in &table.files {
                for digest in std::iter::once(&pair.data_sha256).chain(&pair.delete_sha256) {
                    if !self.shard_cache.has_verified(digest)? {
                        return Err(CompassError::IntegrityViolation(format!(
                            "missing verified shard {digest} in catalog '{}'",
                            self.summary.id
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Load any sealed snapshot by `snapshot_id` (`None` returns `latest_sealed`).
    pub fn load_sealed_snapshot(&self, snapshot_id: Option<u64>) -> CompassResult<SealedSnapshot> {
        match snapshot_id {
            None => Ok(self.summary.latest_sealed.clone()),
            Some(sid) if sid == self.summary.latest_sealed.snapshot_id => {
                Ok(self.summary.latest_sealed.clone())
            }
            Some(sid) if self.shard_cache.selected_tables().is_some() => {
                Err(CompassError::AccessDenied(format!("snapshot {sid} differs from this reader's pin")))
            }
            Some(sid) => {
                let p = self
                    .catalog_dir
                    .join("snapshots")
                    .join(format!("snapshot_{sid}.json"));
                let raw = std::fs::read_to_string(&p).map_err(|_| {
                    CompassError::NotFound(format!(
                        "sealed snapshot {sid} not found or expired for catalog '{}'",
                        self.summary.id
                    ))
                })?;
                let snap: SealedSnapshot = serde_json::from_str(&raw).map_err(|e| {
                    CompassError::Validation(format!("decode snapshot_{sid}.json: {e}"))
                })?;
                let recomputed = SealedSnapshot::compute_dataset_digest(
                    &snap.catalog_id,
                    snap.snapshot_id,
                    &snap.tables,
                );
                if recomputed != snap.dataset_digest {
                    return Err(CompassError::IntegrityViolation(format!(
                        "tampered sealed snapshot {sid} digest mismatch"
                    )));
                }
                Ok(snap)
            }
        }
    }

    /// Create a brand-new local DuckLake catalog, execute `init_sql` (`CREATE TABLE` + `INSERT`),
    /// and seal the initial snapshot.
    #[allow(clippy::too_many_arguments)]
    pub fn create_local_catalog(
        catalog_dir: &Path,
        id: CatalogId,
        name: impl Into<String>,
        version: impl Into<String>,
        description: impl Into<String>,
        organism: impl Into<String>,
        primary_keys: BTreeMap<String, Vec<String>>,
        commit_message: impl Into<String>,
        init_fn: impl FnOnce(&Connection) -> CompassResult<()>,
    ) -> CompassResult<Self> {
        std::fs::create_dir_all(catalog_dir.join("dl_data"))
            .map_err(|e| CompassError::Storage(format!("create dl_data: {e}")))?;
        std::fs::create_dir_all(catalog_dir.join("snapshots"))
            .map_err(|e| CompassError::Storage(format!("create snapshots dir: {e}")))?;
        let shard_cache = ShardCache::open(catalog_dir)?;

        let conn = open_local_ducklake_rw(catalog_dir, "dl")?;
        init_fn(&conn)?;

        let sid = current_ducklake_snapshot_id(&conn, "dl")?;
        let commit_msg = commit_message.into();
        let sealed = seal_ducklake_snapshot(&conn, "dl", &id, sid, &commit_msg, &primary_keys, &shard_cache)?;
        conn.execute_batch("DETACH dl;")
            .map_err(|e| CompassError::Storage(format!("detach dl: {e}")))?;

        write_sealed_snapshot_file(catalog_dir, &sealed)?;

        let snap_summary = SnapshotSummary {
            snapshot_id: sealed.snapshot_id,
            dataset_digest: sealed.dataset_digest.clone(),
            row_count: sealed.total_row_count(),
            created_at: sealed.created_at.clone(),
            commit_message: commit_msg,
            pinned: false,
        };

        let summary = CatalogSummary {
            id,
            name: name.into(),
            version: version.into(),
            description: description.into(),
            organism: organism.into(),
            row_count: sealed.total_row_count(),
            latest_snapshot_id: sealed.snapshot_id,
            dataset_digest: sealed.dataset_digest.clone(),
            tables: sealed.tables.iter().map(|t| t.schema.clone()).collect(),
            facets: sealed.facets.clone(),
            snapshots: vec![snap_summary],
            latest_sealed: sealed,
            scope_relation: None,
        };

        let sidecar = CatalogLocalSidecar {
            primary_keys: primary_keys.clone(),
            pinned_snapshots: BTreeSet::new(),
        };
        save_sidecar(catalog_dir, &sidecar)?;
        write_catalog_summary(catalog_dir, &summary)?;

        Ok(Self {
            catalog_dir: catalog_dir.to_path_buf(),
            shard_cache,
            summary,
            primary_keys,
            pinned_snapshots: BTreeSet::new(),
            read_only: false,
        })
    }

    /// Execute a transactional DuckLake mutation (`INSERT`, `UPDATE`, `DELETE`, `CREATE TABLE` inside `dl.<table_name>`)
    /// and immediately archive & seal the resulting snapshot.
    pub fn commit_mutation(
        &mut self,
        commit_message: impl Into<String>,
        mutate_fn: impl FnOnce(&Connection) -> CompassResult<()>,
    ) -> CompassResult<SealedSnapshot> {
        if self.read_only {
            return Err(CompassError::AccessDenied(
                "cannot mutate a read-only (hospital-edge or cached) DuckLake catalog".into(),
            ));
        }
        let conn = open_local_ducklake_rw(&self.catalog_dir, "dl")?;
        mutate_fn(&conn)?;
        let sid = current_ducklake_snapshot_id(&conn, "dl")?;
        let msg = commit_message.into();
        let sealed = seal_ducklake_snapshot(
            &conn,
            "dl",
            &self.summary.id,
            sid,
            &msg,
            &self.primary_keys,
            &self.shard_cache,
        )?;
        conn.execute_batch("DETACH dl;")
            .map_err(|e| CompassError::Storage(format!("detach dl: {e}")))?;

        self.record_new_sealed_snapshot(sealed.clone(), msg)?;
        Ok(sealed)
    }

    /// Pin a sealed snapshot so compaction/expiry retains its archived shards and manifest (§5.2).
    pub fn pin_snapshot(&mut self, snapshot_id: u64) -> CompassResult<()> {
        if self.read_only {
            return Err(CompassError::AccessDenied(
                "cannot mutate snapshot pins on a read-only catalog".into(),
            ));
        }
        let _ = self.load_sealed_snapshot(Some(snapshot_id))?;
        self.pinned_snapshots.insert(snapshot_id);
        for s in &mut self.summary.snapshots {
            if s.snapshot_id == snapshot_id {
                s.pinned = true;
            }
        }
        self.persist_metadata()?;
        Ok(())
    }

    pub fn unpin_snapshot(&mut self, snapshot_id: u64) -> CompassResult<()> {
        if self.read_only {
            return Err(CompassError::AccessDenied(
                "cannot mutate snapshot pins on a read-only catalog".into(),
            ));
        }
        self.pinned_snapshots.remove(&snapshot_id);
        for s in &mut self.summary.snapshots {
            if s.snapshot_id == snapshot_id {
                s.pinned = false;
            }
        }
        self.persist_metadata()?;
        Ok(())
    }

    /// Expire unpinned historical snapshots older than the `keep_latest` most-recent sealed snapshots,
    /// cleaning up both DuckLake metadata/working files and unreferenced archived shards (§5.2).
    pub fn expire_unpinned_snapshots(&mut self, keep_latest: usize) -> CompassResult<Vec<u64>> {
        if self.read_only {
            return Err(CompassError::AccessDenied(
                "cannot expire snapshots on a read-only catalog".into(),
            ));
        }
        let keep = keep_latest.max(1);
        let total = self.summary.snapshots.len();
        if total <= keep {
            return Ok(Vec::new());
        }
        let cutoff_idx = total - keep;
        let mut to_expire = Vec::new();
        for (idx, snap) in self.summary.snapshots.iter().enumerate() {
            if idx < cutoff_idx
                && !self.pinned_snapshots.contains(&snap.snapshot_id)
                && snap.snapshot_id != self.summary.latest_snapshot_id
            {
                to_expire.push(snap.snapshot_id);
            }
        }
        if to_expire.is_empty() {
            return Ok(Vec::new());
        }

        let conn = open_local_ducklake_rw(&self.catalog_dir, "dl")?;
        let versions_csv = to_expire
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let _ = conn.execute_batch(&format!(
            "CALL ducklake_expire_snapshots('dl', versions => [{versions_csv}]);
             CALL ducklake_cleanup_old_files('dl', cleanup_all => true);"
        ));
        conn.execute_batch("DETACH dl;")
            .map_err(|e| CompassError::Storage(format!("detach dl after expire: {e}")))?;

        let expired_set: BTreeSet<u64> = to_expire.iter().copied().collect();
        self.summary
            .snapshots
            .retain(|s| !expired_set.contains(&s.snapshot_id));
        for sid in &to_expire {
            let p = self
                .catalog_dir
                .join("snapshots")
                .join(format!("snapshot_{sid}.json"));
            let _ = std::fs::remove_file(p);
        }

        // Garbage-collect archived sha256 shards no longer referenced by any retained sealed snapshot.
        let mut retained_digests = BTreeSet::new();
        for s in &self.summary.snapshots {
            if let Ok(sealed) = self.load_sealed_snapshot(Some(s.snapshot_id)) {
                retained_digests.extend(sealed.referenced_digests());
            }
        }
        if let Ok(entries) = std::fs::read_dir(self.shard_cache.sha256_dir()) {
            for entry in entries.flatten() {
                let fname = entry.file_name().to_string_lossy().to_string();
                if let Some(stem) = fname.strip_suffix(".parquet")
                    && stem.len() == 64
                    && !retained_digests.contains(stem)
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }

        self.persist_metadata()?;
        Ok(to_expire)
    }

    /// Roll back the catalog to `target_snapshot_id` by committing a new forward snapshot whose contents
    /// restore the exact sealed state of `target_snapshot_id` (§5.2).
    pub fn rollback_to_snapshot(&mut self, target_snapshot_id: u64) -> CompassResult<SealedSnapshot> {
        if self.read_only {
            return Err(CompassError::AccessDenied(
                "cannot roll back a read-only catalog".into(),
            ));
        }
        let target = self.load_sealed_snapshot(Some(target_snapshot_id))?;
        let shard_cache = self.shard_cache.clone();
        self.commit_mutation(
            format!("Rollback to snapshot {target_snapshot_id}"),
            move |conn| {
                for tbl in &target.tables {
                    let q_tbl = quote_ident(&tbl.schema.name)?;
                    let scan_sql = build_sealed_table_scan_sql(tbl, &shard_cache)?;
                    conn.execute_batch(&format!(
                        "DELETE FROM dl.{q_tbl};
                         INSERT INTO dl.{q_tbl} SELECT * FROM ({scan_sql});"
                    ))
                    .map_err(|e| {
                        CompassError::Storage(format!(
                            "rollback table '{}' to snapshot {target_snapshot_id}: {e}",
                            tbl.schema.name
                        ))
                    })?;
                }
                Ok(())
            },
        )
    }

    /// Execute a schema-validated `QueryAst` under `grant` returning raw Apache Arrow `RecordBatch`es
    /// and `(resolved_snapshot, projected_columns, total_in_scope)`.
    pub fn query_arrow(
        &self,
        grant: &DatasetGrant,
        ast: &QueryAst,
    ) -> CompassResult<ArrowQueryOutput> {
        self.query_multi_arrow(grant, ast, &[])
    }

    /// Execute a `QueryAst` across this catalog plus optional same-plane attached catalogs at pinned snapshots (§5.2).
    pub fn query_multi_arrow(
        &self,
        grant: &DatasetGrant,
        ast: &QueryAst,
        attached: &[(&Self, &DatasetGrant, Option<u64>)],
    ) -> CompassResult<ArrowQueryOutput> {
        self.query_multi_arrow_internal(grant, ast, attached, false)
            .map(|(output, _)| output)
    }

    fn query_multi_arrow_internal(
        &self,
        grant: &DatasetGrant,
        ast: &QueryAst,
        attached: &[(&Self, &DatasetGrant, Option<u64>)],
        track_sources: bool,
    ) -> CompassResult<(ArrowQueryOutput, Option<String>)> {
        if grant.catalog_id != self.summary.id {
            return Err(CompassError::AccessDenied(format!(
                "grant targets catalog '{}', but engine holds '{}'",
                grant.catalog_id, self.summary.id
            )));
        }
        if grant.level != GrantLevel::Rows {
            return Err(CompassError::AccessDenied(format!(
                "catalog '{}' grant '{}' is compute_only; direct row queries are forbidden",
                self.summary.id, grant.grant_id
            )));
        }

        let primary_snap = self.load_sealed_snapshot(ast.snapshot_id)?;
        let primary_tbl = primary_snap.resolve_table(ast.table.as_deref())?;
        let table_name = primary_tbl.schema.name.clone();
        let marker = if track_sources && !attached.is_empty() {
            let name = (0..=primary_tbl.schema.columns.len())
                .map(|i| format!("__biocompass_source_{i}"))
                .find(|name| !primary_tbl.schema.columns.iter().any(|c| c.name.eq_ignore_ascii_case(name)))
                .ok_or_else(|| CompassError::Internal("could not name private source column".into()))?;
            Some(name)
        } else {
            None
        };
        let quoted_marker = marker.as_deref().map(quote_ident).transpose()?;
        let track = |sql: String, index: usize| {
            if let Some(name) = &quoted_marker {
                format!("SELECT *, {index}::BIGINT AS {name} FROM ({sql}) AS quota_source")
            } else {
                sql
            }
        };

        let conn = self.shard_cache.read_connection()?;
        let mut union_views = Vec::with_capacity(1 + attached.len());
        let mut grant_bind_params = Vec::new();

        // Primary catalog grant-scoped subquery
        let (p_sql, p_params) =
            build_grant_scoped_source_sql(primary_tbl, &self.shard_cache, grant)?;
        union_views.push(track(p_sql, 0));
        grant_bind_params.extend(p_params);

        // Additional same-plane attached catalogs
        for (index, (other_eng, other_grant, other_sid)) in attached.iter().enumerate() {
            if other_grant.catalog_id != other_eng.summary.id {
                return Err(CompassError::AccessDenied(format!(
                    "attached grant targets '{}', expected '{}'",
                    other_grant.catalog_id, other_eng.summary.id
                )));
            }
            if other_grant.level != GrantLevel::Rows {
                return Err(CompassError::AccessDenied(format!(
                    "attached catalog '{}' grant is compute_only; direct row queries are forbidden",
                    other_eng.summary.id
                )));
            }
            let other_snap = other_eng.load_sealed_snapshot(*other_sid)?;
            let other_tbl = other_snap.resolve_table(Some(&table_name))?;
            ensure_compatible_schema(&primary_tbl.schema, &other_tbl.schema)?;
            let (o_sql, o_params) =
                build_grant_scoped_source_sql(other_tbl, &other_eng.shard_cache, other_grant)?;
            union_views.push(track(o_sql, index + 1));
            grant_bind_params.extend(o_params);
        }

        let combined_source_sql = union_views.join(" UNION ALL ");
        let (where_sql, filter_params) =
            compile_filter_expr(ast.filter.as_ref(), &primary_tbl.schema)?;

        // Count total matched rows within grant scope
        let mut count_params = grant_bind_params.clone();
        count_params.extend(filter_params.clone());
        let count_sql = format!(
            "SELECT COUNT(*) FROM ({combined_source_sql}) AS scoped_src WHERE {where_sql}"
        );
        let total_in_scope = conn
            .query_row(
                &count_sql,
                params_from_iter(count_params.iter()),
                |r| r.get::<_, i64>(0),
            )
            .map_err(|e| CompassError::Storage(format!("count scoped query: {e}")))?
            as usize;

        // Projected columns (validated against schema)
        let projected_cols = resolve_projected_columns(&ast.columns, &primary_tbl.schema)?;
        let mut select_list = projected_cols
            .iter()
            .map(|c| quote_ident(&c.name))
            .collect::<CompassResult<Vec<_>>>()?
            .join(", ");
        if let Some(name) = &quoted_marker {
            select_list.push_str(", ");
            select_list.push_str(name);
        }

        let order_clause = compile_order_by(&ast.order_by, &primary_tbl.schema)?;

        let mut max_cap = grant.max_rows_per_query;
        for (_, og, _) in attached {
            max_cap = max_cap.min(og.max_rows_per_query);
        }
        let effective_limit = ast
            .limit
            .unwrap_or(100)
            .min(max_cap)
            .min(10_000) as i64;
        let effective_offset = ast.offset.unwrap_or(0) as i64;

        let mut query_params = grant_bind_params;
        query_params.extend(filter_params);
        query_params.push(DuckValue::BigInt(effective_limit));
        query_params.push(DuckValue::BigInt(effective_offset));

        let select_sql = format!(
            "SELECT {select_list} FROM ({combined_source_sql}) AS scoped_src \
             WHERE {where_sql} {order_clause} LIMIT ? OFFSET ?"
        );
        let mut stmt = conn
            .prepare(&select_sql)
            .map_err(|e| CompassError::Storage(format!("prepare Arrow query: {e}")))?;
        let arrow_iter = stmt
            .query_arrow(params_from_iter(query_params.iter()))
            .map_err(|e| CompassError::Storage(format!("execute Arrow query: {e}")))?;
        let batches: Vec<RecordBatch> = arrow_iter.collect();

        Ok(((
            primary_snap,
            table_name,
            projected_cols,
            total_in_scope,
            batches,
        ), marker))
    }

    /// Execute a `QueryAst` under `grant` and serialize Arrow `RecordBatch`es into a JSON `QueryTableResult`.
    pub fn query(
        &self,
        grant: &DatasetGrant,
        ast: &QueryAst,
    ) -> CompassResult<QueryTableResult> {
        self.query_multi(grant, ast, &[])
    }

    pub fn query_multi(
        &self,
        grant: &DatasetGrant,
        ast: &QueryAst,
        attached: &[(&Self, &DatasetGrant, Option<u64>)],
    ) -> CompassResult<QueryTableResult> {
        self.query_multi_json(grant, ast, attached, false).map(|(result, _)| result)
    }

    /// Count only released union rows attributed to each grant after ordering and pagination.
    /// The caller receives the existing public table shape, plus internal counts for settlement.
    pub fn query_multi_with_row_counts(
        &self, grant: &DatasetGrant, ast: &QueryAst, attached: &[(&Self, &DatasetGrant, Option<u64>)],
    ) -> CompassResult<(QueryTableResult, std::collections::BTreeMap<String, u64>)> {
        self.query_multi_json(grant, ast, attached, true)
    }

    fn query_multi_json(
        &self, grant: &DatasetGrant, ast: &QueryAst, attached: &[(&Self, &DatasetGrant, Option<u64>)],
        track_sources: bool,
    ) -> CompassResult<(QueryTableResult, std::collections::BTreeMap<String, u64>)> {
        let ((snap, table, columns, total_in_scope, batches), marker) =
            self.query_multi_arrow_internal(grant, ast, attached, track_sources)?;
        let mut rows = record_batches_to_json_rows(&batches)?;
        let mut counts = std::collections::BTreeMap::new();
        if let Some(marker) = marker {
            let grants: Vec<_> = std::iter::once(grant).chain(attached.iter().map(|(_, grant, _)| *grant)).collect();
            for row in &mut rows {
                let source = row.remove(&marker).and_then(|value| value.as_u64())
                    .and_then(|index| grants.get(index as usize))
                    .ok_or_else(|| CompassError::IntegrityViolation("query row has no valid quota source".into()))?;
                *counts.entry(source.grant_id.clone()).or_insert(0) += 1;
            }
        } else if track_sources {
            counts.insert(grant.grant_id.clone(), rows.len() as u64);
        }
        Ok((QueryTableResult {
            catalog_id: self.summary.id.clone(),
            snapshot_id: snap.snapshot_id,
            dataset_digest: snap.dataset_digest,
            table,
            grant_id: grant.grant_id.clone(),
            access_level: grant.level,
            columns,
            total_in_scope,
            rows,
            remaining_daily_row_budget: grant.daily_row_budget,
        }, counts))
    }

    /// Compute a canonical full-scan SHA-256 `selection_digest` over declared primary-key columns (§7.1).
    pub fn compute_selection_digest(
        &self,
        grant: &DatasetGrant,
        ast: &QueryAst,
    ) -> CompassResult<SelectionDigestResult> {
        self.compute_selection_digest_multi(grant, ast, &[])
    }

    /// Compute a canonical full-scan SHA-256 `selection_digest` across primary + attached catalogs (§7.1).
    /// Fails with `Validation` if the target table lacks a declared primary key, and `AccessDenied`
    /// if any grant is `compute_only`.
    pub fn compute_selection_digest_multi(
        &self,
        grant: &DatasetGrant,
        ast: &QueryAst,
        attached: &[(&Self, &DatasetGrant, Option<u64>)],
    ) -> CompassResult<SelectionDigestResult> {
        if grant.catalog_id != self.summary.id {
            return Err(CompassError::AccessDenied(format!(
                "grant targets catalog '{}', but engine holds '{}'",
                grant.catalog_id, self.summary.id
            )));
        }
        if grant.level != GrantLevel::Rows {
            return Err(CompassError::AccessDenied(format!(
                "catalog '{}' grant '{}' is compute_only; saved views require rows access",
                self.summary.id, grant.grant_id
            )));
        }

        let primary_snap = self.load_sealed_snapshot(ast.snapshot_id)?;
        let primary_tbl = primary_snap.resolve_table(ast.table.as_deref())?;
        let table_name = primary_tbl.schema.name.clone();
        if primary_tbl.schema.primary_key.is_empty() {
            return Err(CompassError::Validation(format!(
                "table '{}' in catalog '{}' has no declared primary_key and cannot be used in a Saved View",
                table_name, self.summary.id
            )));
        }
        let pk_cols = primary_tbl.schema.primary_key.clone();

        let conn = self.shard_cache.read_connection()?;
        let mut union_views = Vec::with_capacity(1 + attached.len());
        let mut bind_params = Vec::new();

        let (p_sql, p_params) =
            build_grant_scoped_source_sql(primary_tbl, &self.shard_cache, grant)?;
        union_views.push(p_sql);
        bind_params.extend(p_params);

        for (other_eng, other_grant, other_sid) in attached {
            if other_grant.catalog_id != other_eng.summary.id {
                return Err(CompassError::AccessDenied(format!(
                    "attached grant targets '{}', expected '{}'",
                    other_grant.catalog_id, other_eng.summary.id
                )));
            }
            if other_grant.level != GrantLevel::Rows {
                return Err(CompassError::AccessDenied(format!(
                    "attached catalog '{}' grant is compute_only; saved views require rows access",
                    other_eng.summary.id
                )));
            }
            let other_snap = other_eng.load_sealed_snapshot(*other_sid)?;
            let other_tbl = other_snap.resolve_table(Some(&table_name))?;
            ensure_compatible_schema(&primary_tbl.schema, &other_tbl.schema)?;
            let (o_sql, o_params) =
                build_grant_scoped_source_sql(other_tbl, &other_eng.shard_cache, other_grant)?;
            union_views.push(o_sql);
            bind_params.extend(o_params);
        }

        let combined_source_sql = union_views.join(" UNION ALL ");
        let (where_sql, filter_params) =
            compile_filter_expr(ast.filter.as_ref(), &primary_tbl.schema)?;
        bind_params.extend(filter_params);

        let mut pk_selects = Vec::with_capacity(pk_cols.len());
        let mut pk_orders = Vec::with_capacity(pk_cols.len());
        for pk in &pk_cols {
            let q_pk = quote_ident(pk)?;
            pk_selects.push(format!("CAST({q_pk} AS VARCHAR)"));
            pk_orders.push(format!("{q_pk} ASC NULLS FIRST"));
        }

        let sql = format!(
            "SELECT {} FROM ({combined_source_sql}) AS scoped_src WHERE {where_sql} ORDER BY {}",
            pk_selects.join(", "),
            pk_orders.join(", ")
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| CompassError::Storage(format!("prepare selection_digest scan: {e}")))?;
        let pk_len = pk_cols.len();
        let mut rows = stmt
            .query(params_from_iter(bind_params.iter()))
            .map_err(|e| CompassError::Storage(format!("execute selection_digest scan: {e}")))?;

        let mut hasher = Sha256::new();
        hasher.update(b"biocompass-saved-view-selection:v2|");
        hasher.update(table_name.as_bytes());
        hasher.update(b"|");
        hasher.update(pk_cols.join(",").as_bytes());
        hasher.update(b"\n");

        let mut row_count: u64 = 0;
        while let Some(r) = rows
            .next()
            .map_err(|e| CompassError::Storage(format!("read PK row: {e}")))?
        {
            hasher.update(b"row:");
            for idx in 0..pk_len {
                let val: Option<String> = r
                    .get(idx)
                    .map_err(|e| CompassError::Storage(format!("decode PK cell {idx}: {e}")))?;
                match val {
                    Some(s) => {
                        hasher.update(b"v:");
                        hasher.update(s.len().to_string().as_bytes());
                        hasher.update(b":");
                        hasher.update(s.as_bytes());
                        hasher.update(b";");
                    }
                    None => hasher.update(b"null;"),
                }
            }
            hasher.update(b"\n");
            row_count += 1;
        }

        Ok(SelectionDigestResult {
            catalog_id: self.summary.id.clone(),
            table_name,
            primary_key_columns: pk_cols,
            snapshot_id: primary_snap.snapshot_id,
            dataset_digest: primary_snap.dataset_digest,
            row_count,
            selection_digest: hex::encode(hasher.finalize()),
        })
    }

    /// Import a Stage-1 validated Parquet file into a local DuckLake catalog (`append | replace | edit`, §7.2).
    pub fn import_normalized_parquet(
        &mut self,
        table_name: &str,
        mode: IngestMode,
        primary_key: &[String],
        normalized_parquet: &Path,
        commit_message: impl Into<String>,
    ) -> CompassResult<SealedSnapshot> {
        self.summary.id.ensure_legacy_writable()?;
        if self.read_only {
            return Err(CompassError::AccessDenied(
                "cannot import into a read-only catalog".into(),
            ));
        }
        let clean_tbl = validate_sql_ident(table_name)?.to_owned();
        if !primary_key.is_empty() {
            self.primary_keys
                .insert(clean_tbl.clone(), primary_key.to_vec());
        }
        let effective_pk = self
            .primary_keys
            .get(&clean_tbl)
            .cloned()
            .unwrap_or_else(|| primary_key.to_vec());
        let pq_path = normalized_parquet.to_path_buf();
        self.commit_mutation(commit_message, move |conn| {
            apply_normalized_parquet_to_ducklake(
                conn,
                "dl",
                &pq_path,
                &clean_tbl,
                mode,
                &effective_pk,
            )
        })
    }

    /// Create a brand-new local DuckLake catalog or import into an existing one from a normalized Parquet file (§7.2).
    #[allow(clippy::too_many_arguments)]
    pub fn create_or_import_local_parquet(
        catalog_dir: &Path,
        catalog_id: CatalogId,
        catalog_name: &str,
        catalog_description: &str,
        organism: &str,
        table_name: &str,
        mode: IngestMode,
        primary_key: &[String],
        normalized_parquet: &Path,
        commit_message: &str,
    ) -> CompassResult<Self> {
        catalog_id.ensure_legacy_writable()?;
        if catalog_dir.join("catalog.json").is_file() {
            let mut eng = Self::open_verified(catalog_dir)?;
            let _ = eng.import_normalized_parquet(
                table_name,
                mode,
                primary_key,
                normalized_parquet,
                commit_message,
            )?;
            return Ok(eng);
        }
        let clean_tbl = validate_sql_ident(table_name)?.to_owned();
        let mut pks = BTreeMap::new();
        if !primary_key.is_empty() {
            pks.insert(clean_tbl.clone(), primary_key.to_vec());
        }
        let pk_vec = primary_key.to_vec();
        let pq_path = normalized_parquet.to_path_buf();
        Self::create_local_catalog(
            catalog_dir,
            catalog_id,
            catalog_name,
            "1.0.0",
            catalog_description,
            organism,
            pks,
            commit_message,
            move |conn| {
                apply_normalized_parquet_to_ducklake(
                    conn,
                    "dl",
                    &pq_path,
                    &clean_tbl,
                    mode,
                    &pk_vec,
                )
            },
        )
    }

    /// Stage a Parquet slice into `dest_parquet` (`/in/data/cohort.parquet`) with strict fail-loud
    /// full-scan semantics (`B11` fix: NEVER truncates silently!).
    pub fn materialize_parquet_for_run(
        &self,
        grant: &DatasetGrant,
        ast: &QueryAst,
        mode: StagingMode,
        dest_parquet: &Path,
    ) -> CompassResult<MaterializedSlice> {
        if grant.catalog_id != self.summary.id {
            return Err(CompassError::AccessDenied(format!(
                "grant targets catalog '{}', but engine holds '{}'",
                grant.catalog_id, self.summary.id
            )));
        }
        if mode == StagingMode::CustomerScoped && grant.level == GrantLevel::ComputeOnly {
            return Err(CompassError::AccessDenied(format!(
                "custom or unapproved module cannot stage rows from compute_only catalog '{}'",
                self.summary.id
            )));
        }

        let snap = self.load_sealed_snapshot(ast.snapshot_id)?;
        let tbl = snap.resolve_table(ast.table.as_deref())?;

        if let Some(parent) = dest_parquet.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CompassError::Storage(format!("create staging dir: {e}")))?;
        }

        let conn = open_parquet_only_duckdb()?;
        let (source_sql, grant_params) =
            build_grant_scoped_source_sql(tbl, &self.shard_cache, grant)?;
        let (where_sql, filter_params) = compile_filter_expr(ast.filter.as_ref(), &tbl.schema)?;

        let mut all_params = grant_params;
        all_params.extend(filter_params);

        let expected_rows: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM ({source_sql}) AS s WHERE {where_sql}"),
                params_from_iter(all_params.iter()),
                |r| r.get(0),
            )
            .map_err(|e| CompassError::Storage(format!("count stageable rows: {e}")))?;
        let expected_u64 = expected_rows.max(0) as u64;

        // B11: CustomerScoped runs fail loudly if matched rows exceed grant.max_rows_per_query (NEVER silent LIMIT).
        if mode == StagingMode::CustomerScoped && expected_u64 > grant.max_rows_per_query as u64 {
            return Err(CompassError::AccessDenied(format!(
                "staging matched {expected_u64} rows in '{}', exceeding grant max_rows_per_query ({}); narrow your filter instead of truncating",
                self.summary.id, grant.max_rows_per_query
            )));
        }
        if let Some(explicit_limit) = ast.limit
            && expected_u64 > explicit_limit as u64
        {
            return Err(CompassError::Validation(format!(
                "staging matched {expected_u64} rows, exceeding requested limit ({explicit_limit}); full-scan staging never truncates silently"
            )));
        }

        let order_clause = if !ast.order_by.is_empty() {
            compile_order_by(&ast.order_by, &tbl.schema)?
        } else if let Some(first_pk) = tbl.schema.primary_key.first() {
            format!("ORDER BY {} ASC", quote_ident(first_pk)?)
        } else {
            String::new()
        };

        conn.execute(
            &format!(
                "CREATE TEMP TABLE materialized_stage AS \
                 SELECT * FROM ({source_sql}) AS s WHERE {where_sql} {order_clause}"
            ),
            params_from_iter(all_params.iter()),
        )
        .map_err(|e| CompassError::Storage(format!("stage materialized_stage: {e}")))?;

        let escaped_dest = sql_quote_internal_path(&dest_parquet.to_string_lossy());
        conn.execute_batch(&format!(
            "COPY materialized_stage TO '{escaped_dest}' (FORMAT PARQUET, COMPRESSION ZSTD);"
        ))
        .map_err(|e| CompassError::Storage(format!("export staged parquet: {e}")))?;

        let written_rows: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM read_parquet('{escaped_dest}')"),
                [],
                |r| r.get(0),
            )
            .map_err(|e| CompassError::Storage(format!("verify staged parquet row count: {e}")))?;
        if written_rows != expected_rows {
            return Err(CompassError::IntegrityViolation(format!(
                "staged parquet row count mismatch: expected {expected_rows}, wrote {written_rows}"
            )));
        }

        let bytes = std::fs::read(dest_parquet)
            .map_err(|e| CompassError::Storage(format!("hash staged parquet: {e}")))?;
        let sha256 = hex::encode(Sha256::digest(&bytes));

        Ok(MaterializedSlice {
            path: dest_parquet.to_path_buf(),
            row_count: written_rows as u64,
            sha256,
            snapshot_id: snap.snapshot_id,
            dataset_digest: snap.dataset_digest,
        })
    }

    /// Backwards-compatible wrapper defaulting to `StagingMode::CuratedFullScan` when `grant.level == ComputeOnly`
    /// is caller-approved or `StagingMode::CustomerScoped` otherwise — prefer `materialize_parquet_for_run`.
    pub fn materialize_scoped_parquet(
        &self,
        grant: &DatasetGrant,
        ast: &QueryAst,
        dest_parquet: &Path,
    ) -> CompassResult<MaterializedSlice> {
        self.materialize_parquet_for_run(grant, ast, StagingMode::CuratedFullScan, dest_parquet)
    }

    fn record_new_sealed_snapshot(
        &mut self,
        sealed: SealedSnapshot,
        commit_message: String,
    ) -> CompassResult<()> {
        write_sealed_snapshot_file(&self.catalog_dir, &sealed)?;
        let snap_summary = SnapshotSummary {
            snapshot_id: sealed.snapshot_id,
            dataset_digest: sealed.dataset_digest.clone(),
            row_count: sealed.total_row_count(),
            created_at: sealed.created_at.clone(),
            commit_message,
            pinned: self.pinned_snapshots.contains(&sealed.snapshot_id),
        };
        self.summary.row_count = sealed.total_row_count();
        self.summary.latest_snapshot_id = sealed.snapshot_id;
        self.summary.dataset_digest = sealed.dataset_digest.clone();
        self.summary.tables = sealed.tables.iter().map(|t| t.schema.clone()).collect();
        self.summary.facets = sealed.facets.clone();
        self.summary.snapshots.push(snap_summary);
        self.summary.latest_sealed = sealed;
        self.persist_metadata()
    }

    fn persist_metadata(&self) -> CompassResult<()> {
        let sidecar = CatalogLocalSidecar {
            primary_keys: self.primary_keys.clone(),
            pinned_snapshots: self.pinned_snapshots.clone(),
        };
        save_sidecar(&self.catalog_dir, &sidecar)?;
        write_catalog_summary(&self.catalog_dir, &self.summary)
    }
}

/// Open a cloud DuckLake catalog attached to isolated `biocompass_ducklake` Postgres + S3 (`DATA_INLINING_ROW_LIMIT 0`),
/// run `mutate_fn`, seal the snapshot into `shard_cache` (returning the newly archived shard paths for S3 upload),
/// and detach cleanly releasing the Postgres connection.
pub fn commit_and_seal_cloud_ducklake(
    cfg: &CloudDuckLakeConfig,
    catalog_id: &CatalogId,
    primary_keys: &BTreeMap<String, Vec<String>>,
    commit_message: &str,
    shard_cache: &ShardCache,
    mutate_fn: impl FnOnce(&Connection) -> CompassResult<()>,
) -> CompassResult<SealedSnapshot> {
    let conn = open_duckdb_with_extensions(&["httpfs", "postgres_scanner", "ducklake"])?;
    let endpoint = cfg
        .s3_endpoint
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    conn.execute_batch(&format!(
        "CREATE SECRET cloud_dl_s3 (
            TYPE s3,
            KEY_ID '{}',
            SECRET '{}',
            REGION '{}',
            ENDPOINT '{}',
            URL_STYLE 'path',
            USE_SSL {}
        );",
        sql_quote_internal_path(&cfg.s3_access_key),
        sql_quote_internal_path(&cfg.s3_secret_key),
        sql_quote_internal_path(&cfg.s3_region),
        sql_quote_internal_path(endpoint),
        if cfg.s3_use_ssl { "true" } else { "false" }
    ))
    .map_err(|e| CompassError::Storage(format!("configure DuckLake S3 secret: {e}")))?;

    let dsn = sql_quote_internal_path(&cfg.postgres_dsn);
    let schema = validate_sql_ident(&cfg.metadata_schema)?;
    let s3_data = format!(
        "s3://{}/{}/dl_data/",
        cfg.s3_bucket.trim_matches('/'),
        cfg.data_prefix.trim_matches('/')
    );
    conn.execute_batch(&format!(
        "ATTACH 'ducklake:postgres:{dsn}' AS cloud_dl (METADATA_SCHEMA '{schema}', DATA_PATH '{}', DATA_INLINING_ROW_LIMIT 0);",
        sql_quote_internal_path(&s3_data)
    ))
    .map_err(|e| CompassError::Storage(format!("attach cloud DuckLake: {e}")))?;

    mutate_fn(&conn)?;

    let sid = current_ducklake_snapshot_id(&conn, "cloud_dl")?;
    let sealed = seal_ducklake_snapshot(
        &conn,
        "cloud_dl",
        catalog_id,
        sid,
        commit_message,
        primary_keys,
        shard_cache,
    )?;
    let _ = conn.execute_batch("DETACH cloud_dl;");
    Ok(sealed)
}

/// Typed, SQL-free cloud DuckLake mutation operations executed on `froglet-node` (`§4.5` / `§5.2`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum CloudMutationSpec {
    SeedDemo {
        catalog_id: CatalogId,
    },
    SeedPublicDemo {
        catalog_id: CatalogId,
    },
    AppendRows {
        #[serde(default)]
        table: Option<String>,
        rows: Vec<Map<String, Value>>,
        #[serde(default)]
        commit_message: String,
    },
    UpdateWhere {
        #[serde(default)]
        table: Option<String>,
        filter: FilterExpr,
        assignments: BTreeMap<String, ScalarValue>,
        #[serde(default)]
        commit_message: String,
    },
    DeleteWhere {
        #[serde(default)]
        table: Option<String>,
        filter: FilterExpr,
        #[serde(default)]
        commit_message: String,
    },
    Rollback {
        target_snapshot_id: u64,
    },
    ExpireClaimedSnapshots {
        snapshot_ids: BTreeSet<u64>,
    },
    ImportNormalizedParquet {
        catalog_id: CatalogId,
        #[serde(default)]
        catalog_name: Option<String>,
        #[serde(default)]
        catalog_description: Option<String>,
        #[serde(default)]
        organism: Option<String>,
        table_name: String,
        mode: IngestMode,
        #[serde(default)]
        primary_key: Vec<String>,
        normalized_parquet_path: String,
        #[serde(default)]
        commit_message: String,
    },
}

/// Result of applying a `CloudMutationSpec` on `froglet-node` against `biocompass_ducklake` + S3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudMutationOutcome {
    pub summary: CatalogSummary,
    #[serde(default)]
    pub new_sealed: Option<SealedSnapshot>,
    #[serde(default)]
    pub expired_snapshot_ids: Vec<u64>,
}

fn json_cell_to_duck_value(val: &Value) -> CompassResult<DuckValue> {
    match val {
        Value::Null => Ok(DuckValue::Null),
        Value::Bool(b) => Ok(DuckValue::Boolean(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(DuckValue::BigInt(i))
            } else if let Some(u) = n.as_u64() {
                Ok(DuckValue::UBigInt(u))
            } else if let Some(f) = n.as_f64() {
                Ok(DuckValue::Double(f))
            } else {
                Err(CompassError::Validation(format!("unsupported number '{n}'")))
            }
        }
        Value::String(s) => Ok(DuckValue::Text(s.clone())),
        other => Err(CompassError::Validation(format!(
            "unsupported non-scalar JSON value '{other}' in row mutation"
        ))),
    }
}

/// Execute a typed `CloudMutationSpec` against isolated `biocompass_ducklake` Postgres + S3 (`DATA_INLINING_ROW_LIMIT 0`),
/// archive newly written shards into `shard_cache`, and return the updated `CatalogSummary` + `SealedSnapshot`.
pub fn execute_cloud_ducklake_mutation(
    cfg: &CloudDuckLakeConfig,
    current_summary: Option<&CatalogSummary>,
    spec: &CloudMutationSpec,
    shard_cache: &ShardCache,
    target_sealed_for_rollback: Option<&SealedSnapshot>,
) -> CompassResult<CloudMutationOutcome> {
    if let Some(current) = current_summary {
        current.id.ensure_legacy_writable()?;
    }
    if let CloudMutationSpec::SeedDemo { catalog_id }
        | CloudMutationSpec::SeedPublicDemo { catalog_id }
        | CloudMutationSpec::ImportNormalizedParquet { catalog_id, .. } = spec
    {
        catalog_id.ensure_legacy_writable()?;
    }
    match spec {
        CloudMutationSpec::SeedDemo { catalog_id } => {
            let mut primary_keys = BTreeMap::new();
            primary_keys.insert("peptides".to_string(), vec!["peptide_id".to_string()]);
            let commit_msg = "Initial sealed v2026.1.0 cohort";
            let sealed = commit_and_seal_cloud_ducklake(
                cfg,
                catalog_id,
                &primary_keys,
                commit_msg,
                shard_cache,
                |conn| seed::populate_demo_peptides_table(conn, "cloud_dl"),
            )?;
            let snap_summary = SnapshotSummary {
                snapshot_id: sealed.snapshot_id,
                dataset_digest: sealed.dataset_digest.clone(),
                row_count: sealed.total_row_count(),
                created_at: sealed.created_at.clone(),
                commit_message: commit_msg.to_owned(),
                pinned: false,
            };
            let summary = CatalogSummary {
                id: catalog_id.clone(),
                name: "Alithea HLA-I Multi-Tissue Immunopeptidomics Atlas".into(),
                version: "2026.1.0".into(),
                description: "LC-MS/MS eluted HLA Class I ligandome across Lung NSCLC, Primary Melanoma, CRC, and healthy human tissues with NetMHCpan-4.1 binding affinities.".into(),
                organism: "Homo sapiens (GRCh38)".into(),
                row_count: sealed.total_row_count(),
                latest_snapshot_id: sealed.snapshot_id,
                dataset_digest: sealed.dataset_digest.clone(),
                tables: sealed.tables.iter().map(|t| t.schema.clone()).collect(),
                facets: sealed.facets.clone(),
                snapshots: vec![snap_summary],
                latest_sealed: sealed.clone(),
                scope_relation: None,
            };
            Ok(CloudMutationOutcome {
                summary,
                new_sealed: Some(sealed),
                expired_snapshot_ids: Vec::new(),
            })
        }
        CloudMutationSpec::SeedPublicDemo { catalog_id } => {
            let mut primary_keys = BTreeMap::new();
            primary_keys.insert("peptides".to_string(), vec!["peptide_id".to_string()]);
            let commit_msg = "Initial sealed public CEF benchmark demo v1.0.0";
            let sealed = commit_and_seal_cloud_ducklake(
                cfg,
                catalog_id,
                &primary_keys,
                commit_msg,
                shard_cache,
                |conn| seed::populate_public_demo_peptides_table(conn, "cloud_dl"),
            )?;
            let snap_summary = SnapshotSummary {
                snapshot_id: sealed.snapshot_id,
                dataset_digest: sealed.dataset_digest.clone(),
                row_count: sealed.total_row_count(),
                created_at: sealed.created_at.clone(),
                commit_message: commit_msg.to_owned(),
                pinned: false,
            };
            let summary = CatalogSummary {
                id: catalog_id.clone(),
                name: "Alithea Public HLA-I Viral Benchmark Demo".into(),
                version: "1.0.0".into(),
                description: "Synthetic public CEF viral control epitopes (PBMC & Lymph Node) for self-serve evaluation — physically isolated from proprietary clinical atlases.".into(),
                organism: "Homo sapiens (Public CEF Controls)".into(),
                row_count: sealed.total_row_count(),
                latest_snapshot_id: sealed.snapshot_id,
                dataset_digest: sealed.dataset_digest.clone(),
                tables: sealed.tables.iter().map(|t| t.schema.clone()).collect(),
                facets: sealed.facets.clone(),
                snapshots: vec![snap_summary],
                latest_sealed: sealed.clone(),
                scope_relation: None,
            };
            Ok(CloudMutationOutcome {
                summary,
                new_sealed: Some(sealed),
                expired_snapshot_ids: Vec::new(),
            })
        }
        CloudMutationSpec::ExpireClaimedSnapshots { snapshot_ids } => {
            let cur = current_summary.ok_or_else(|| {
                CompassError::Validation("current_summary required for snapshot expiry".into())
            })?;
            if snapshot_ids.contains(&cur.latest_snapshot_id) {
                return Err(CompassError::IntegrityViolation(
                    "cannot expire the current snapshot".into(),
                ));
            }
            let conn = open_duckdb_with_extensions(&["httpfs", "postgres_scanner", "ducklake"])?;
            let endpoint = cfg
                .s3_endpoint
                .trim_start_matches("http://")
                .trim_start_matches("https://")
                .trim_end_matches('/');
            conn.execute_batch(&format!(
                "CREATE SECRET cloud_dl_s3 (
                    TYPE s3,
                    KEY_ID '{}',
                    SECRET '{}',
                    REGION '{}',
                    ENDPOINT '{}',
                    URL_STYLE 'path',
                    USE_SSL {}
                );",
                sql_quote_internal_path(&cfg.s3_access_key),
                sql_quote_internal_path(&cfg.s3_secret_key),
                sql_quote_internal_path(&cfg.s3_region),
                sql_quote_internal_path(endpoint),
                if cfg.s3_use_ssl { "true" } else { "false" }
            ))
            .map_err(|e| CompassError::Storage(format!("configure DuckLake S3 secret: {e}")))?;
            let dsn = sql_quote_internal_path(&cfg.postgres_dsn);
            let schema = validate_sql_ident(&cfg.metadata_schema)?;
            let s3_data = format!(
                "s3://{}/{}/dl_data/",
                cfg.s3_bucket.trim_matches('/'),
                cfg.data_prefix.trim_matches('/')
            );
            conn.execute_batch(&format!(
                "ATTACH 'ducklake:postgres:{dsn}' AS cloud_dl (METADATA_SCHEMA '{schema}', DATA_PATH '{}', DATA_INLINING_ROW_LIMIT 0);",
                sql_quote_internal_path(&s3_data)
            ))
            .map_err(|e| CompassError::Storage(format!("attach cloud DuckLake for expire: {e}")))?;
            expire_claimed_versions(&conn, snapshot_ids)?;
            let mut next = cur.clone();
            next.snapshots
                .retain(|s| !snapshot_ids.contains(&s.snapshot_id));
            Ok(CloudMutationOutcome {
                summary: next,
                new_sealed: None,
                expired_snapshot_ids: snapshot_ids.iter().copied().collect(),
            })
        }
        CloudMutationSpec::ImportNormalizedParquet {
            catalog_id,
            catalog_name,
            catalog_description,
            organism,
            table_name,
            mode,
            primary_key,
            normalized_parquet_path,
            commit_message,
        } => {
            let clean_tbl = validate_sql_ident(table_name)?.to_owned();
            let mut primary_keys = BTreeMap::new();
            if let Some(cur) = current_summary {
                for t in &cur.tables {
                    if !t.primary_key.is_empty() {
                        primary_keys.insert(t.name.clone(), t.primary_key.clone());
                    }
                }
            }
            if !primary_key.is_empty() {
                primary_keys.insert(clean_tbl.clone(), primary_key.clone());
            }
            let effective_pk = primary_keys
                .get(&clean_tbl)
                .cloned()
                .unwrap_or_else(|| primary_key.clone());
            let msg = if commit_message.trim().is_empty() {
                format!("Import {} into table '{}'", mode.as_str(), clean_tbl)
            } else {
                commit_message.clone()
            };
            let pq_path = PathBuf::from(normalized_parquet_path);
            let sealed = commit_and_seal_cloud_ducklake(
                cfg,
                catalog_id,
                &primary_keys,
                &msg,
                shard_cache,
                |conn| {
                    apply_normalized_parquet_to_ducklake(
                        conn,
                        "cloud_dl",
                        &pq_path,
                        &clean_tbl,
                        *mode,
                        &effective_pk,
                    )
                },
            )?;
            let snap_summary = SnapshotSummary {
                snapshot_id: sealed.snapshot_id,
                dataset_digest: sealed.dataset_digest.clone(),
                row_count: sealed.total_row_count(),
                created_at: sealed.created_at.clone(),
                commit_message: msg,
                pinned: false,
            };
            let summary = if let Some(cur) = current_summary {
                let mut next = cur.clone();
                next.row_count = sealed.total_row_count();
                next.latest_snapshot_id = sealed.snapshot_id;
                next.dataset_digest = sealed.dataset_digest.clone();
                next.tables = sealed.tables.iter().map(|t| t.schema.clone()).collect();
                next.facets = sealed.facets.clone();
                next.snapshots.push(snap_summary);
                next.latest_sealed = sealed.clone();
                next
            } else {
                CatalogSummary {
                    id: catalog_id.clone(),
                    name: catalog_name
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| catalog_id.as_str().to_owned()),
                    version: "1.0.0".into(),
                    description: catalog_description
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| {
                            "Curated DuckLake tabular catalog imported via two-stage quarantine pipeline.".into()
                        }),
                    organism: organism
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| "Unspecified".into()),
                    row_count: sealed.total_row_count(),
                    latest_snapshot_id: sealed.snapshot_id,
                    dataset_digest: sealed.dataset_digest.clone(),
                    tables: sealed.tables.iter().map(|t| t.schema.clone()).collect(),
                    facets: sealed.facets.clone(),
                    snapshots: vec![snap_summary],
                    latest_sealed: sealed.clone(),
                    scope_relation: None,
                }
            };
            Ok(CloudMutationOutcome {
                summary,
                new_sealed: Some(sealed),
                expired_snapshot_ids: Vec::new(),
            })
        }
        other => {
            let cur = current_summary.ok_or_else(|| {
                CompassError::Validation("current_summary required for catalog mutation".into())
            })?;
            let mut primary_keys = BTreeMap::new();
            for t in &cur.tables {
                if !t.primary_key.is_empty() {
                    primary_keys.insert(t.name.clone(), t.primary_key.clone());
                }
            }
            let commit_msg = match other {
                CloudMutationSpec::AppendRows { commit_message, rows, .. } => {
                    if commit_message.trim().is_empty() {
                        format!("Append {} row(s)", rows.len())
                    } else {
                        commit_message.clone()
                    }
                }
                CloudMutationSpec::UpdateWhere { commit_message, .. } => {
                    if commit_message.trim().is_empty() {
                        "Update rows".into()
                    } else {
                        commit_message.clone()
                    }
                }
                CloudMutationSpec::DeleteWhere { commit_message, .. } => {
                    if commit_message.trim().is_empty() {
                        "Delete rows".into()
                    } else {
                        commit_message.clone()
                    }
                }
                CloudMutationSpec::Rollback { target_snapshot_id } => {
                    format!("Rollback to snapshot {target_snapshot_id}")
                }
                _ => unreachable!(),
            };

            let sealed = commit_and_seal_cloud_ducklake(
                cfg,
                &cur.id,
                &primary_keys,
                &commit_msg,
                shard_cache,
                |conn| match other {
                    CloudMutationSpec::AppendRows { table, rows, .. } => {
                        if rows.is_empty() {
                            return Err(CompassError::Validation("AppendRows requires at least 1 row".into()));
                        }
                        let tbl = cur.latest_sealed.resolve_table(table.as_deref())?;
                        let q_tbl = quote_ident(&tbl.schema.name)?;
                        let col_list = tbl
                            .schema
                            .columns
                            .iter()
                            .map(|c| quote_ident(&c.name))
                            .collect::<CompassResult<Vec<_>>>()?
                            .join(", ");
                        let placeholders = vec!["?"; tbl.schema.columns.len()].join(", ");
                        let sql = format!("INSERT INTO cloud_dl.{q_tbl} ({col_list}) VALUES ({placeholders})");
                        let mut stmt = conn
                            .prepare(&sql)
                            .map_err(|e| CompassError::Storage(format!("prepare AppendRows: {e}")))?;
                        for row_map in rows {
                            let mut vals = Vec::with_capacity(tbl.schema.columns.len());
                            for col in &tbl.schema.columns {
                                let cell = row_map.get(&col.name).unwrap_or(&Value::Null);
                                vals.push(json_cell_to_duck_value(cell)?);
                            }
                            stmt.execute(params_from_iter(vals.iter()))
                                .map_err(|e| CompassError::Storage(format!("execute AppendRows: {e}")))?;
                        }
                        Ok(())
                    }
                    CloudMutationSpec::UpdateWhere {
                        table,
                        filter,
                        assignments,
                        ..
                    } => {
                        if assignments.is_empty() {
                            return Err(CompassError::Validation("UpdateWhere requires assignments".into()));
                        }
                        let tbl = cur.latest_sealed.resolve_table(table.as_deref())?;
                        let q_tbl = quote_ident(&tbl.schema.name)?;
                        let mut set_clauses = Vec::with_capacity(assignments.len());
                        let mut bind_vals = Vec::new();
                        for (col_name, val) in assignments {
                            let col = find_schema_column(col_name, &tbl.schema)?;
                            set_clauses.push(format!("{} = ?", quote_ident(&col.name)?));
                            bind_vals.push(scalar_to_duck_value(val)?);
                        }
                        let (where_sql, filter_vals) = compile_filter_expr(Some(filter), &tbl.schema)?;
                        bind_vals.extend(filter_vals);
                        let sql = format!(
                            "UPDATE cloud_dl.{q_tbl} SET {} WHERE {where_sql}",
                            set_clauses.join(", ")
                        );
                        conn.execute(&sql, params_from_iter(bind_vals.iter()))
                            .map_err(|e| CompassError::Storage(format!("execute UpdateWhere: {e}")))?;
                        Ok(())
                    }
                    CloudMutationSpec::DeleteWhere { table, filter, .. } => {
                        let tbl = cur.latest_sealed.resolve_table(table.as_deref())?;
                        let q_tbl = quote_ident(&tbl.schema.name)?;
                        let (where_sql, filter_vals) = compile_filter_expr(Some(filter), &tbl.schema)?;
                        let sql = format!("DELETE FROM cloud_dl.{q_tbl} WHERE {where_sql}");
                        conn.execute(&sql, params_from_iter(filter_vals.iter()))
                            .map_err(|e| CompassError::Storage(format!("execute DeleteWhere: {e}")))?;
                        Ok(())
                    }
                    CloudMutationSpec::Rollback { target_snapshot_id } => {
                        let target = target_sealed_for_rollback.ok_or_else(|| {
                            CompassError::NotFound(format!(
                                "target sealed snapshot {target_snapshot_id} not loaded for rollback"
                            ))
                        })?;
                        for tbl in &target.tables {
                            let q_tbl = quote_ident(&tbl.schema.name)?;
                            let scan_sql = build_sealed_table_scan_sql(tbl, shard_cache)?;
                            conn.execute_batch(&format!(
                                "DELETE FROM cloud_dl.{q_tbl};
                                 INSERT INTO cloud_dl.{q_tbl} SELECT * FROM ({scan_sql});"
                            ))
                            .map_err(|e| {
                                CompassError::Storage(format!(
                                    "rollback cloud_dl.{q_tbl} to snapshot {target_snapshot_id}: {e}"
                                ))
                            })?;
                        }
                        Ok(())
                    }
                    _ => unreachable!(),
                },
            )?;

            let mut next = cur.clone();
            next.row_count = sealed.total_row_count();
            next.latest_snapshot_id = sealed.snapshot_id;
            next.dataset_digest = sealed.dataset_digest.clone();
            next.tables = sealed.tables.iter().map(|t| t.schema.clone()).collect();
            next.facets = sealed.facets.clone();
            next.snapshots.push(SnapshotSummary {
                snapshot_id: sealed.snapshot_id,
                dataset_digest: sealed.dataset_digest.clone(),
                row_count: sealed.total_row_count(),
                created_at: sealed.created_at.clone(),
                commit_message: commit_msg,
                pinned: false,
            });
            next.latest_sealed = sealed.clone();

            Ok(CloudMutationOutcome {
                summary: next,
                new_sealed: Some(sealed),
                expired_snapshot_ids: Vec::new(),
            })
        }
    }
}

/// Apply a Stage-1 validated Parquet file into an attached DuckLake catalog (`dl` or `cloud_dl`)
/// under `append`, `replace`, or `edit` (PK upsert) semantics (§7.2).
pub fn apply_normalized_parquet_to_ducklake(
    conn: &Connection,
    dl_alias: &str,
    normalized_parquet: &Path,
    table_name: &str,
    mode: IngestMode,
    primary_key: &[String],
) -> CompassResult<()> {
    let q_alias = validate_sql_ident(dl_alias)?;
    let escaped_alias = sql_quote_internal_path(dl_alias);
    let clean_tbl = validate_sql_ident(table_name)?.to_owned();
    let q_tbl = quote_ident(&clean_tbl)?;
    let escaped_pq = sql_quote_internal_path(&normalized_parquet.to_string_lossy());

    let existing_cols: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT column_name FROM duckdb_columns() \
                 WHERE database_name = ? AND schema_name = 'main' AND table_name = ? \
                 ORDER BY column_index",
            )
            .map_err(|e| CompassError::Storage(format!("inspect DuckLake columns: {e}")))?;
        let mapped = stmt
            .query_map([escaped_alias.as_str(), clean_tbl.as_str()], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|e| CompassError::Storage(format!("query DuckLake columns: {e}")))?;
        let mut cols = Vec::new();
        for c in mapped {
            cols.push(c.map_err(|e| CompassError::Storage(format!("column name: {e}")))?);
        }
        cols
    };

    if existing_cols.is_empty() {
        if mode == IngestMode::Edit && primary_key.is_empty() {
            return Err(CompassError::Validation(
                "ingest mode 'edit' requires a non-empty primary_key".into(),
            ));
        }
        conn.execute_batch(&format!(
            "CREATE TABLE {q_alias}.{q_tbl} AS SELECT * FROM read_parquet('{escaped_pq}');"
        ))
        .map_err(|e| CompassError::Storage(format!("create DuckLake table {clean_tbl}: {e}")))?;
        return Ok(());
    }

    // Inspect incoming Parquet columns
    let incoming_cols: Vec<String> = {
        let mut stmt = conn
            .prepare(&format!("DESCRIBE SELECT * FROM read_parquet('{escaped_pq}')"))
            .map_err(|e| CompassError::Storage(format!("describe normalized parquet: {e}")))?;
        let mapped = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| CompassError::Storage(format!("read normalized parquet cols: {e}")))?;
        let mut cols = Vec::new();
        for c in mapped {
            cols.push(c.map_err(|e| CompassError::Storage(format!("parquet col: {e}")))?);
        }
        cols
    };

    match mode {
        IngestMode::Replace => {
            if incoming_cols == existing_cols {
                let col_list = existing_cols
                    .iter()
                    .map(|c| quote_ident(c))
                    .collect::<CompassResult<Vec<_>>>()?
                    .join(", ");
                conn.execute_batch(&format!(
                    "DELETE FROM {q_alias}.{q_tbl};
                     INSERT INTO {q_alias}.{q_tbl} ({col_list}) SELECT {col_list} FROM read_parquet('{escaped_pq}');"
                ))
                .map_err(|e| CompassError::Storage(format!("replace DuckLake table {clean_tbl}: {e}")))?;
            } else {
                conn.execute_batch(&format!(
                    "DROP TABLE {q_alias}.{q_tbl};
                     CREATE TABLE {q_alias}.{q_tbl} AS SELECT * FROM read_parquet('{escaped_pq}');"
                ))
                .map_err(|e| CompassError::Storage(format!("recreate DuckLake table {clean_tbl}: {e}")))?;
            }
        }
        IngestMode::Append => {
            for ec in &existing_cols {
                if !incoming_cols.iter().any(|ic| ic == ec) {
                    return Err(CompassError::Validation(format!(
                        "append missing required column '{ec}' of table '{clean_tbl}'"
                    )));
                }
            }
            if !primary_key.is_empty() {
                let mut join_conds = Vec::with_capacity(primary_key.len());
                for pk in primary_key {
                    let q_pk = quote_ident(pk)?;
                    join_conds.push(format!("existing.{q_pk} = incoming.{q_pk}"));
                }
                let collisions: i64 = conn
                    .query_row(
                        &format!(
                            "SELECT COUNT(*) FROM {q_alias}.{q_tbl} AS existing \
                             INNER JOIN read_parquet('{escaped_pq}') AS incoming \
                             ON {}",
                            join_conds.join(" AND ")
                        ),
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| CompassError::Storage(format!("check append PK collisions: {e}")))?;
                if collisions > 0 {
                    return Err(CompassError::IntegrityViolation(format!(
                        "append rejected: {collisions} incoming row(s) collide with existing primary key(s) in '{clean_tbl}'; use mode='edit' to upsert"
                    )));
                }
            }
            let col_list = existing_cols
                .iter()
                .map(|c| quote_ident(c))
                .collect::<CompassResult<Vec<_>>>()?
                .join(", ");
            conn.execute_batch(&format!(
                "INSERT INTO {q_alias}.{q_tbl} ({col_list}) SELECT {col_list} FROM read_parquet('{escaped_pq}');"
            ))
            .map_err(|e| CompassError::Storage(format!("append into DuckLake table {clean_tbl}: {e}")))?;
        }
        IngestMode::Edit => {
            if primary_key.is_empty() {
                return Err(CompassError::Validation(format!(
                    "edit (upsert) on table '{clean_tbl}' requires a declared primary_key"
                )));
            }
            for ec in &existing_cols {
                if !incoming_cols.iter().any(|ic| ic == ec) {
                    return Err(CompassError::Validation(format!(
                        "edit missing required column '{ec}' of table '{clean_tbl}'"
                    )));
                }
            }
            let mut match_conds = Vec::with_capacity(primary_key.len());
            for pk in primary_key {
                let q_pk = quote_ident(pk)?;
                match_conds.push(format!("{q_alias}.{q_tbl}.{q_pk} = incoming.{q_pk}"));
            }
            let col_list = existing_cols
                .iter()
                .map(|c| quote_ident(c))
                .collect::<CompassResult<Vec<_>>>()?
                .join(", ");
            conn.execute_batch(&format!(
                "DELETE FROM {q_alias}.{q_tbl} WHERE EXISTS (
                     SELECT 1 FROM read_parquet('{escaped_pq}') AS incoming WHERE {}
                 );
                 INSERT INTO {q_alias}.{q_tbl} ({col_list}) SELECT {col_list} FROM read_parquet('{escaped_pq}');",
                match_conds.join(" AND ")
            ))
            .map_err(|e| CompassError::Storage(format!("edit/upsert DuckLake table {clean_tbl}: {e}")))?;
        }
    }
    Ok(())
}

/// Stage 1 Airgapped Quarantine Validator (§7.2):
/// Parses `/in/ingest_spec.json` + `/in/quarantine/<filename>` (`Parquet | Csv | Tsv`) with zero
/// network or cloud credentials, enforces schema & primary-key uniqueness/non-null invariants,
/// and writes normalized zstd `/out/normalized.parquet` + `/out/ingest_report.json`.
pub fn validate_and_normalize_quarantine_dir(
    in_dir: &Path,
    out_dir: &Path,
) -> CompassResult<QuarantineValidationReport> {
    let spec_path = in_dir.join("ingest_spec.json");
    let spec_raw = std::fs::read_to_string(&spec_path).map_err(|e| {
        CompassError::Validation(format!("read {}: {e}", spec_path.display()))
    })?;
    let spec: QuarantineIngestSpec = serde_json::from_str(&spec_raw)
        .map_err(|e| CompassError::Validation(format!("decode ingest_spec.json: {e}")))?;

    let clean_tbl = validate_sql_ident(&spec.table_name)?.to_owned();
    let clean_fname = spec.filename.trim();
    if clean_fname.is_empty()
        || clean_fname.contains('/')
        || clean_fname.contains('\\')
        || clean_fname == "."
        || clean_fname == ".."
        || clean_fname.contains('\0')
    {
        return Err(CompassError::AccessDenied(format!(
            "invalid quarantine filename '{clean_fname}'"
        )));
    }
    let input_file = in_dir.join("quarantine").join(clean_fname);
    if !input_file.is_file() {
        return Err(CompassError::NotFound(format!(
            "quarantine input file {} does not exist",
            input_file.display()
        )));
    }

    std::fs::create_dir_all(out_dir)
        .map_err(|e| CompassError::Storage(format!("create out_dir {}: {e}", out_dir.display())))?;

    let conn = open_parquet_only_duckdb()?;
    let escaped_in = sql_quote_internal_path(&input_file.to_string_lossy());
    let reader_expr = match spec.format {
        IngestFormat::Parquet => format!("read_parquet('{escaped_in}')"),
        IngestFormat::Csv => {
            format!("read_csv('{escaped_in}', delim = ',', header = true, auto_detect = true)")
        }
        IngestFormat::Tsv => {
            format!("read_csv('{escaped_in}', delim = '\t', header = true, auto_detect = true)")
        }
    };

    conn.execute_batch(&format!(
        "CREATE TEMP TABLE quarantine_raw AS SELECT * FROM {reader_expr};"
    ))
    .map_err(|e| {
        CompassError::Validation(format!(
            "quarantine {} validation failed on '{}': {e}",
            spec.format.as_str(),
            clean_fname
        ))
    })?;

    // Inspect parsed columns
    let mut parsed_cols: Vec<ColumnDescriptor> = {
        let mut stmt = conn
            .prepare("DESCRIBE quarantine_raw")
            .map_err(|e| CompassError::Storage(format!("describe quarantine_raw: {e}")))?;
        let mapped = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2).unwrap_or_else(|_| "YES".into()),
                ))
            })
            .map_err(|e| CompassError::Storage(format!("read quarantine_raw schema: {e}")))?;
        let mut out = Vec::new();
        for item in mapped {
            let (name, dtype, null_str) =
                item.map_err(|e| CompassError::Storage(format!("quarantine_raw col: {e}")))?;
            let _ = validate_sql_ident(&name)?;
            out.push(ColumnDescriptor {
                name,
                data_type: dtype,
                nullable: !null_str.eq_ignore_ascii_case("NO"),
            });
        }
        out
    };

    if parsed_cols.is_empty() {
        return Err(CompassError::Validation(
            "uploaded table contains zero columns".into(),
        ));
    }

    let row_count_i64: i64 = conn
        .query_row("SELECT COUNT(*) FROM quarantine_raw", [], |r| r.get(0))
        .map_err(|e| CompassError::Storage(format!("count quarantine_raw: {e}")))?;
    if row_count_i64 <= 0 {
        return Err(CompassError::Validation(
            "uploaded table contains zero data rows".into(),
        ));
    }
    let row_count = row_count_i64 as u64;

    // Resolve effective primary key (explicit spec.primary_key or existing table PK)
    let effective_pk = if !spec.primary_key.is_empty() {
        spec.primary_key.clone()
    } else if let Some(ref existing) = spec.expected_schema {
        existing.primary_key.clone()
    } else {
        Vec::new()
    };

    if spec.mode == IngestMode::Edit && effective_pk.is_empty() {
        return Err(CompassError::Validation(
            "ingest mode 'edit' requires at least one primary_key column".into(),
        ));
    }

    // Validate PK existence, non-null, and uniqueness within uploaded batch
    if !effective_pk.is_empty() {
        let mut q_pks = Vec::with_capacity(effective_pk.len());
        for pk in &effective_pk {
            if !parsed_cols.iter().any(|c| &c.name == pk) {
                return Err(CompassError::Validation(format!(
                    "declared primary_key column '{pk}' not found in uploaded columns {:?}",
                    parsed_cols.iter().map(|c| &c.name).collect::<Vec<_>>()
                )));
            }
            let q_pk = quote_ident(pk)?;
            let null_cnt: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM quarantine_raw WHERE {q_pk} IS NULL"),
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| CompassError::Storage(format!("check PK nulls: {e}")))?;
            if null_cnt > 0 {
                return Err(CompassError::Validation(format!(
                    "primary_key column '{pk}' contains {null_cnt} NULL value(s)"
                )));
            }
            q_pks.push(q_pk);
        }

        let dup_groups: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM (SELECT 1 FROM quarantine_raw GROUP BY {} HAVING COUNT(*) > 1)",
                    q_pks.join(", ")
                ),
                [],
                |r| r.get(0),
            )
            .map_err(|e| CompassError::Storage(format!("check PK duplicates: {e}")))?;
        if dup_groups > 0 {
            return Err(CompassError::Validation(format!(
                "uploaded file contains {dup_groups} duplicate primary_key key(s) on ({})",
                effective_pk.join(", ")
            )));
        }
    }

    // If appending or editing an existing table, cast & validate against expected_schema!
    let export_source_table = if let Some(ref existing) = spec.expected_schema
        && matches!(spec.mode, IngestMode::Append | IngestMode::Edit)
    {
        let mut cast_exprs = Vec::with_capacity(existing.columns.len());
        for ecol in &existing.columns {
            if !parsed_cols.iter().any(|c| c.name == ecol.name) {
                return Err(CompassError::Validation(format!(
                    "uploaded file is missing expected column '{}' (type {}) for table '{}'",
                    ecol.name, ecol.data_type, clean_tbl
                )));
            }
            let q_col = quote_ident(&ecol.name)?;
            // DuckDB spells the all-null Parquet logical type as a quoted type name.
            // Only that exact name (possibly nested in lists) may contain quotes.
            let null_type = ecol.data_type.trim_end_matches("[]") == r#""NULL""#;
            if !null_type
                && !ecol.data_type.chars().all(|ch| {
                    ch.is_ascii_alphanumeric()
                        || matches!(ch, ' ' | '(' | ')' | ',' | '_' | '[' | ']')
                })
            {
                return Err(CompassError::Validation(format!(
                    "unsafe expected column data_type '{}'",
                    ecol.data_type
                )));
            }
            cast_exprs.push(format!("CAST({q_col} AS {}) AS {q_col}", ecol.data_type));
        }
        conn.execute_batch(&format!(
            "CREATE TEMP TABLE quarantine_normalized AS SELECT {} FROM quarantine_raw;",
            cast_exprs.join(", ")
        ))
        .map_err(|e| {
            CompassError::Validation(format!(
                "type mismatch against existing schema of '{clean_tbl}': {e}"
            ))
        })?;
        parsed_cols = existing.columns.clone();
        "quarantine_normalized"
    } else {
        "quarantine_raw"
    };

    let out_pq = out_dir.join("normalized.parquet");
    let escaped_out = sql_quote_internal_path(&out_pq.to_string_lossy());
    conn.execute_batch(&format!(
        "COPY {export_source_table} TO '{escaped_out}' (FORMAT PARQUET, COMPRESSION ZSTD);"
    ))
    .map_err(|e| CompassError::Storage(format!("write normalized.parquet: {e}")))?;

    let pq_bytes = std::fs::read(&out_pq)
        .map_err(|e| CompassError::Storage(format!("read normalized.parquet: {e}")))?;
    let normalized_bytes = pq_bytes.len() as u64;
    let normalized_sha256 = hex::encode(Sha256::digest(&pq_bytes));

    let report = QuarantineValidationReport {
        table_name: clean_tbl,
        format: spec.format,
        mode: spec.mode,
        row_count,
        columns: parsed_cols,
        primary_key: effective_pk,
        normalized_bytes,
        normalized_sha256,
    };
    let report_json = serde_json::to_vec_pretty(&report)
        .map_err(|e| CompassError::Internal(format!("encode QuarantineValidationReport: {e}")))?;
    std::fs::write(out_dir.join("ingest_report.json"), report_json)
        .map_err(|e| CompassError::Storage(format!("write ingest_report.json: {e}")))?;

    Ok(report)
}

/// Convert Apache Arrow `RecordBatch`es into JSON row maps at the HTTP/WS API edge (§5.2).
pub fn record_batches_to_json_rows(batches: &[RecordBatch]) -> CompassResult<Vec<Map<String, Value>>> {
    let total_rows: usize = batches.iter().map(RecordBatch::num_rows).sum();
    let mut out = Vec::with_capacity(total_rows);
    let fmt_opts = FormatOptions::default();

    for batch in batches {
        let schema = batch.schema();
        let num_rows = batch.num_rows();
        let num_cols = batch.num_columns();

        // Pre-build fallback formatters for complex column types once per batch.
        let formatters: Vec<Option<ArrayFormatter<'_>>> = (0..num_cols)
            .map(|c| ArrayFormatter::try_new(batch.column(c).as_ref(), &fmt_opts).ok())
            .collect();

        for row_idx in 0..num_rows {
            let mut obj = Map::with_capacity(num_cols);
            for (col_idx, formatter) in formatters.iter().enumerate() {
                let field_name = schema.field(col_idx).name().clone();
                let col = batch.column(col_idx);
                let val = arrow_cell_to_json(col.as_ref(), row_idx, formatter.as_ref())?;
                obj.insert(field_name, val);
            }
            out.push(obj);
        }
    }
    Ok(out)
}

/// Raw Arrow query output: `(resolved_snapshot, table_name, projected_columns, total_in_scope, batches)`.
pub type ArrowQueryOutput = (
    SealedSnapshot,
    String,
    Vec<ColumnDescriptor>,
    usize,
    Vec<RecordBatch>,
);

fn arrow_cell_to_json(
    col: &dyn Array,
    row_idx: usize,
    formatter: Option<&ArrayFormatter<'_>>,
) -> CompassResult<Value> {
    if col.is_null(row_idx) {
        return Ok(Value::Null);
    }
    match col.data_type() {
        DataType::Null => Ok(Value::Null),
        DataType::List(_) => list_to_json(col.as_list::<i32>().value(row_idx).as_ref()),
        DataType::LargeList(_) => list_to_json(col.as_list::<i64>().value(row_idx).as_ref()),
        DataType::ListView(_) => list_to_json(col.as_list_view::<i32>().value(row_idx).as_ref()),
        DataType::LargeListView(_) => {
            list_to_json(col.as_list_view::<i64>().value(row_idx).as_ref())
        }
        DataType::FixedSizeList(_, _) => {
            list_to_json(col.as_fixed_size_list().value(row_idx).as_ref())
        }
        DataType::Struct(fields) => {
            let mut object = Map::with_capacity(fields.len());
            for (field, values) in fields.iter().zip(col.as_struct().columns()) {
                object.insert(
                    field.name().clone(),
                    arrow_cell_to_json(values.as_ref(), row_idx, None)?,
                );
            }
            Ok(Value::Object(object))
        }
        DataType::Map(_, _) => {
            let entries = col.as_map().value(row_idx);
            let keys = entries.column(0);
            let values = entries.column(1);
            if matches!(
                keys.data_type(),
                DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View
            ) {
                let mut object = Map::with_capacity(entries.len());
                for i in 0..entries.len() {
                    let Value::String(key) = arrow_cell_to_json(keys.as_ref(), i, None)? else {
                        return Err(CompassError::Internal("null Arrow map key".into()));
                    };
                    if object
                        .insert(key, arrow_cell_to_json(values.as_ref(), i, None)?)
                        .is_some()
                    {
                        return Err(CompassError::Internal("duplicate Arrow map key".into()));
                    }
                }
                Ok(Value::Object(object))
            } else {
                // JSON object keys must be strings. Preserve the typed keys in the
                // Arrow entry records for maps with another key type.
                list_to_json(&entries)
            }
        }
        DataType::Boolean => {
            let a = col.as_any().downcast_ref::<BooleanArray>().unwrap();
            Ok(Value::Bool(a.value(row_idx)))
        }
        DataType::Int8 => {
            let a = col.as_any().downcast_ref::<Int8Array>().unwrap();
            Ok(Value::from(a.value(row_idx)))
        }
        DataType::Int16 => {
            let a = col.as_any().downcast_ref::<Int16Array>().unwrap();
            Ok(Value::from(a.value(row_idx)))
        }
        DataType::Int32 => {
            let a = col.as_any().downcast_ref::<Int32Array>().unwrap();
            Ok(Value::from(a.value(row_idx)))
        }
        DataType::Int64 => {
            let a = col.as_any().downcast_ref::<Int64Array>().unwrap();
            Ok(Value::from(a.value(row_idx)))
        }
        DataType::UInt8 => {
            let a = col.as_any().downcast_ref::<UInt8Array>().unwrap();
            Ok(Value::from(a.value(row_idx)))
        }
        DataType::UInt16 => {
            let a = col.as_any().downcast_ref::<UInt16Array>().unwrap();
            Ok(Value::from(a.value(row_idx)))
        }
        DataType::UInt32 => {
            let a = col.as_any().downcast_ref::<UInt32Array>().unwrap();
            Ok(Value::from(a.value(row_idx)))
        }
        DataType::UInt64 => {
            let a = col.as_any().downcast_ref::<UInt64Array>().unwrap();
            Ok(Value::from(a.value(row_idx)))
        }
        DataType::Float32 => {
            let a = col.as_any().downcast_ref::<Float32Array>().unwrap();
            let v = f64::from(a.value(row_idx));
            Ok(serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number))
        }
        DataType::Float64 => {
            let a = col.as_any().downcast_ref::<Float64Array>().unwrap();
            let v = a.value(row_idx);
            Ok(serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number))
        }
        DataType::Utf8 => {
            let a = col.as_any().downcast_ref::<StringArray>().unwrap();
            Ok(Value::String(a.value(row_idx).to_owned()))
        }
        DataType::LargeUtf8 => {
            let a = col.as_any().downcast_ref::<LargeStringArray>().unwrap();
            Ok(Value::String(a.value(row_idx).to_owned()))
        }
        DataType::Utf8View => {
            let a = col.as_any().downcast_ref::<StringViewArray>().unwrap();
            Ok(Value::String(a.value(row_idx).to_owned()))
        }
        _ => match formatter {
            Some(fmt) => Ok(Value::String(fmt.value(row_idx).to_string())),
            None => array_value_to_string(col, row_idx)
                .map(Value::String)
                .map_err(|e| {
                    CompassError::Internal(format!(
                        "format Arrow column {:?}: {e}",
                        col.data_type()
                    ))
                }),
        },
    }
}

fn list_to_json(values: &dyn Array) -> CompassResult<Value> {
    (0..values.len())
        .map(|i| arrow_cell_to_json(values, i, None))
        .collect::<CompassResult<Vec<_>>>()
        .map(Value::Array)
}

fn open_parquet_only_duckdb() -> CompassResult<Connection> {
    let parquet_path = resolve_vendored_extension_path("parquet")?;
    let mut cfg = Config::default()
        .with("autoinstall_known_extensions", "false")
        .map_err(|e| CompassError::Storage(format!("duckdb config autoinstall=false: {e}")))?
        .with("autoload_known_extensions", "false")
        .map_err(|e| CompassError::Storage(format!("duckdb config autoload=false: {e}")))?;
    if let Some(ext_root) = parquet_path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
    {
        cfg = cfg
            .with("extension_directory", ext_root.to_string_lossy().as_ref())
            .map_err(|e| CompassError::Storage(format!("duckdb config extension_directory: {e}")))?;
    }
    let conn = Connection::open_in_memory_with_flags(cfg)
        .map_err(|e| CompassError::Storage(format!("open in-memory duckdb: {e}")))?;
    let escaped_parquet = sql_quote_internal_path(&parquet_path.to_string_lossy());
    conn.execute_batch(&format!("LOAD '{escaped_parquet}';"))
        .map_err(|e| {
            CompassError::Storage(format!(
                "failed offline signed LOAD of DuckDB extension 'parquet' from {}: {e}",
                parquet_path.display()
            ))
        })?;
    Ok(conn)
}

pub fn open_duckdb_with_extensions(exts: &[&str]) -> CompassResult<Connection> {
    let conn = open_parquet_only_duckdb()?;
    let unsigned: String = conn
        .query_row(
            "SELECT current_setting('allow_unsigned_extensions')::VARCHAR",
            [],
            |r| r.get(0),
        )
        .map_err(|e| CompassError::Storage(format!("verify allow_unsigned_extensions: {e}")))?;
    if unsigned != "false" {
        return Err(CompassError::IntegrityViolation(
            "DuckDB allow_unsigned_extensions must be false".into(),
        ));
    }
    for ext in exts {
        let p = resolve_vendored_extension_path(ext)?;
        let escaped = sql_quote_internal_path(&p.to_string_lossy());
        conn.execute_batch(&format!("LOAD '{escaped}';")).map_err(|e| {
            CompassError::Storage(format!(
                "failed offline signed LOAD of DuckDB extension '{ext}' from {}: {e}",
                p.display()
            ))
        })?;
    }
    Ok(conn)
}

fn resolve_vendored_extension_path(ext: &str) -> CompassResult<PathBuf> {
    let platform = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "osx_arm64"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "linux_arm64"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux_amd64"
    } else {
        return Err(CompassError::Validation(
            "unsupported platform for vendored DuckDB extensions".into(),
        ));
    };

    let rel = PathBuf::from("v2.0.0")
        .join(platform)
        .join(format!("{ext}.duckdb_extension"));

    let mut roots = Vec::new();
    if let Ok(env_dir) = std::env::var("BIOCOMPASS_DUCKDB_EXTENSION_DIR")
        && !env_dir.trim().is_empty()
    {
        roots.push(PathBuf::from(env_dir));
    }
    roots.push(PathBuf::from("/opt/biocompass/duckdb-extensions"));
    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        roots.push(exe_dir.join("duckdb-extensions"));
        for anc in exe_dir.ancestors().take(5) {
            roots.push(anc.join("vendor").join("duckdb-extensions"));
        }
    }
    let compile_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(ws) = compile_root.parent().and_then(Path::parent) {
        roots.push(ws.join("vendor").join("duckdb-extensions"));
    }

    for root in &roots {
        let candidate = root.join(&rel);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(CompassError::NotFound(format!(
        "vendored signed DuckDB extension '{}' not found (searched {:?})",
        rel.display(),
        roots
    )))
}

fn open_local_ducklake_rw(catalog_dir: &Path, alias: &str) -> CompassResult<Connection> {
    let conn = open_duckdb_with_extensions(&["ducklake"])?;
    let meta = catalog_dir.join("meta.ducklake");
    let data = catalog_dir.join("dl_data");
    std::fs::create_dir_all(&data)
        .map_err(|e| CompassError::Storage(format!("mkdir {}: {e}", data.display())))?;
    let q_alias = validate_sql_ident(alias)?;
    conn.execute_batch(&format!(
        "ATTACH 'ducklake:{}' AS {q_alias} (DATA_PATH '{}', DATA_INLINING_ROW_LIMIT 0);",
        sql_quote_internal_path(&meta.to_string_lossy()),
        sql_quote_internal_path(&data.to_string_lossy())
    ))
    .map_err(|e| CompassError::Storage(format!("attach local DuckLake {}: {e}", meta.display())))?;
    Ok(conn)
}

/// Expire the exact, already-fenced versions. A retry may follow a successful expiry and a lost reply.
fn expire_claimed_versions(conn: &Connection, requested: &BTreeSet<u64>) -> CompassResult<()> {
    let mut stmt = conn
        .prepare("SELECT snapshot_id FROM ducklake_snapshots('cloud_dl')")
        .map_err(|e| CompassError::Storage(format!("list DuckLake versions: {e}")))?;
    let present = stmt
        .query_map([], |row| row.get::<_, u64>(0))
        .and_then(|ids| ids.collect::<Result<BTreeSet<_>, _>>())
        .map_err(|e| CompassError::Storage(format!("read DuckLake versions: {e}")))?;
    drop(stmt);
    if present.last().is_some_and(|id| requested.contains(id)) {
        return Err(CompassError::IntegrityViolation(
            "cannot physically expire the current DuckLake version".into(),
        ));
    }
    let versions = requested
        .intersection(&present)
        .map(u64::to_string)
        .collect::<Vec<_>>();
    if !versions.is_empty() {
        let versions_csv = versions.join(", ");
        conn.execute_batch(&format!(
            "CALL ducklake_expire_snapshots('cloud_dl', versions => [{versions_csv}])"
        ))
        .map_err(|e| CompassError::Storage(format!("expire selected DuckLake versions: {e}")))?;
    }
    conn.execute_batch("CALL ducklake_cleanup_old_files('cloud_dl', cleanup_all => true)")
        .map_err(|e| CompassError::Storage(format!("clean expired DuckLake files: {e}")))
}

fn current_ducklake_snapshot_id(conn: &Connection, alias: &str) -> CompassResult<u64> {
    let escaped = sql_quote_internal_path(alias);
    let sid: i64 = conn
        .query_row(
            &format!("SELECT id FROM ducklake_current_snapshot('{escaped}')"),
            [],
            |r| r.get(0),
        )
        .or_else(|_| {
            conn.query_row(
                &format!("SELECT COALESCE(MAX(snapshot_id), 0) FROM ducklake_snapshots('{escaped}')"),
                [],
                |r| r.get(0),
            )
        })
        .map_err(|e| CompassError::Storage(format!("query ducklake_current_snapshot: {e}")))?;
    Ok(sid.max(0) as u64)
}

fn seal_ducklake_snapshot(
    conn: &Connection,
    alias: &str,
    catalog_id: &CatalogId,
    snapshot_id: u64,
    commit_message: &str,
    primary_keys: &BTreeMap<String, Vec<String>>,
    shard_cache: &ShardCache,
) -> CompassResult<SealedSnapshot> {
    let q_alias = validate_sql_ident(alias)?;
    let escaped_alias = sql_quote_internal_path(alias);

    // Enumerate user tables in `main` schema of the attached DuckLake catalog.
    let table_names: Vec<String> = {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT table_name FROM duckdb_tables() \
                 WHERE database_name = '{escaped_alias}' AND schema_name = 'main' \
                 ORDER BY table_name"
            ))
            .map_err(|e| CompassError::Storage(format!("list DuckLake tables: {e}")))?;
        let mapped = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| CompassError::Storage(format!("read DuckLake tables: {e}")))?;
        let mut names = Vec::new();
        for item in mapped {
            names.push(item.map_err(|e| CompassError::Storage(format!("table name: {e}")))?);
        }
        names
    };

    let mut sealed_tables = Vec::with_capacity(table_names.len());
    let mut facets: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for tbl_name in table_names {
        let q_tbl = quote_ident(&tbl_name)?;
        let escaped_tbl = sql_quote_internal_path(&tbl_name);

        // Column schema at `snapshot_id`
        let columns: Vec<ColumnDescriptor> = {
            let mut cstmt = conn
                .prepare(&format!(
                    "DESCRIBE SELECT * FROM {q_alias}.{q_tbl} AT (VERSION => {snapshot_id})"
                ))
                .map_err(|e| CompassError::Storage(format!("describe {tbl_name}: {e}")))?;
            let mapped = cstmt
                .query_map([], |r| {
                    let name: String = r.get(0)?;
                    let data_type: String = r.get(1)?;
                    let null_str: String = r.get(2)?;
                    Ok(ColumnDescriptor {
                        name,
                        data_type,
                        nullable: null_str.eq_ignore_ascii_case("YES"),
                    })
                })
                .map_err(|e| CompassError::Storage(format!("read columns of {tbl_name}: {e}")))?;
            let mut cols = Vec::new();
            for item in mapped {
                cols.push(item.map_err(|e| CompassError::Storage(format!("col spec: {e}")))?);
            }
            cols
        };

        let row_count: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM {q_alias}.{q_tbl} AT (VERSION => {snapshot_id})"
                ),
                [],
                |r| r.get(0),
            )
            .map_err(|e| CompassError::Storage(format!("count {tbl_name}: {e}")))?;

        let pk = primary_keys.get(&tbl_name).cloned().unwrap_or_default();
        for pk_col in &pk {
            if !columns.iter().any(|c| &c.name == pk_col) {
                return Err(CompassError::Validation(format!(
                    "declared primary_key column '{pk_col}' missing from table '{tbl_name}'"
                )));
            }
        }

        // Enumerate active (data_file, delete_file) at `snapshot_id` and archive by SHA-256 into `shard_cache`!
        let file_rows: Vec<(String, Option<String>)> = {
            let mut fstmt = conn
                .prepare(&format!(
                    "SELECT data_file, delete_file \
                     FROM ducklake_list_files('{escaped_alias}', '{escaped_tbl}', snapshot_version => {snapshot_id}) \
                     ORDER BY data_file"
                ))
                .map_err(|e| CompassError::Storage(format!("ducklake_list_files({tbl_name}): {e}")))?;
            let mapped = fstmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))
                .map_err(|e| CompassError::Storage(format!("read file list({tbl_name}): {e}")))?;
            let mut list = Vec::new();
            for item in mapped {
                list.push(item.map_err(|e| CompassError::Storage(format!("file pair: {e}")))?);
            }
            list
        };

        let mut sealed_files = Vec::with_capacity(file_rows.len());
        for (data_ref, del_ref) in file_rows {
            let (data_sha256, data_size_bytes) =
                archive_ducklake_path(conn, &data_ref, shard_cache)?;
            let (delete_sha256, delete_size_bytes) = match del_ref {
                Some(dref) => {
                    let (d_sha, d_sz) = archive_ducklake_path(conn, &dref, shard_cache)?;
                    (Some(d_sha), Some(d_sz))
                }
                None => (None, None),
            };
            sealed_files.push(SealedFilePair {
                data_sha256,
                data_size_bytes,
                delete_sha256,
                delete_size_bytes,
            });
        }

        // Compute non-sensitive facet lists (e.g., `hla_alleles`, `organs`, `conditions`)
        for &facet_col in DEFAULT_FACET_COLUMNS {
            if columns.iter().any(|c| c.name == facet_col) {
                let q_fcol = quote_ident(facet_col)?;
                let mut fstmt = conn
                    .prepare(&format!(
                        "SELECT DISTINCT {q_fcol}::VARCHAR FROM {q_alias}.{q_tbl} AT (VERSION => {snapshot_id}) \
                         WHERE {q_fcol} IS NOT NULL ORDER BY 1 LIMIT 500"
                    ))
                    .map_err(|e| CompassError::Storage(format!("facet {facet_col}: {e}")))?;
                let vals: Vec<String> = fstmt
                    .query_map([], |r| r.get::<_, String>(0))
                    .map_err(|e| CompassError::Storage(format!("read facet {facet_col}: {e}")))?
                    .filter_map(Result::ok)
                    .filter(|s| !s.trim().is_empty())
                    .collect();
                let plural_key = match facet_col {
                    "hla_allele" => "hla_alleles".to_string(),
                    "organ" => "organs".to_string(),
                    "condition" => "conditions".to_string(),
                    other => format!("{other}s"),
                };
                let entry = facets.entry(plural_key).or_default();
                for v in vals {
                    if !entry.contains(&v) {
                        entry.push(v);
                    }
                }
                entry.sort();
            }
        }

        sealed_tables.push(SealedTableSnapshot {
            schema: TableSchema {
                name: tbl_name,
                columns,
                primary_key: pk,
                row_count: row_count.max(0) as u64,
            },
            files: sealed_files,
        });
    }

    let dataset_digest =
        SealedSnapshot::compute_dataset_digest(catalog_id, snapshot_id, &sealed_tables);

    Ok(SealedSnapshot {
        catalog_id: catalog_id.clone(),
        snapshot_id,
        dataset_digest,
        created_at: Utc::now().to_rfc3339(),
        commit_message: commit_message.to_owned(),
        tables: sealed_tables,
        facets,
    })
}

/// Archive either a local filesystem Parquet file or an `s3://` DuckLake Parquet file into `ShardCache`.
fn archive_ducklake_path(
    conn: &Connection,
    path_or_uri: &str,
    shard_cache: &ShardCache,
) -> CompassResult<(String, u64)> {
    if path_or_uri.starts_with("s3://") {
        let tmp = shard_cache
            .sha256_dir()
            .join(format!(".dl-fetch-{}.parquet", Uuid::new_v4().simple()));
        let escaped_uri = sql_quote_internal_path(path_or_uri);
        let escaped_tmp = sql_quote_internal_path(&tmp.to_string_lossy());
        // Read exact raw bytes from S3 via DuckDB's `read_blob` so byte-for-byte Parquet + footer stays intact!
        let bytes: Vec<u8> = conn
            .query_row(
                &format!("SELECT content FROM read_blob('{escaped_uri}')"),
                [],
                |r| r.get(0),
            )
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                CompassError::Storage(format!("fetch S3 DuckLake shard {path_or_uri}: {e}"))
            })?;
        let _ = escaped_tmp;
        let digest = hex::encode(Sha256::digest(&bytes));
        let size = bytes.len() as u64;
        shard_cache.ingest_bytes(&digest, &bytes)?;
        Ok((digest, size))
    } else {
        shard_cache.archive_new_file(Path::new(path_or_uri))
    }
}

/// Check the parts of the sealed schema rendered by `build_sealed_table_scan_sql` without opening any file.
pub fn validate_table_scan_schema(table: &SealedTableSnapshot) -> CompassResult<()> {
    if table.schema.columns.is_empty() {
        return Err(CompassError::Validation(format!(
            "cannot scan table '{}' without columns", table.schema.name
        )));
    }
    for column in &table.schema.columns {
        validate_sql_ident(&column.name)?;
        // Nonempty tables obtain their types from Parquet. Only typed empty scans emit these names into SQL.
        if table.files.is_empty() {
            sanitize_duckdb_type(&column.data_type)?;
        }
    }
    Ok(())
}

/// Validate a single-source module cohort query using only its registered snapshot and grant.
pub fn validate_run_query(
    snapshot: &SealedSnapshot,
    grant: &DatasetGrant,
    ast: &QueryAst,
) -> CompassResult<()> {
    if snapshot.catalog_id != grant.catalog_id {
        return Err(CompassError::AccessDenied("run grant targets another catalog".into()));
    }
    if ast.snapshot_id.is_some_and(|sid| sid != snapshot.snapshot_id) || !ast.additional_sources.is_empty() {
        return Err(CompassError::Validation("run query must use the selected single snapshot".into()));
    }
    let table = snapshot.resolve_table(ast.table.as_deref())?;
    validate_table_scan_schema(table)?;
    let schema = &table.schema;
    resolve_projected_columns(&ast.columns, schema)?;
    compile_filter_expr(grant.row_scope.as_ref(), schema)?;
    compile_filter_expr(ast.filter.as_ref(), schema)?;
    compile_order_by(&ast.order_by, schema)?;
    Ok(())
}

/// Build a read-only query over one sealed table's cached data, accounting for position deletes.
pub fn build_sealed_table_scan_sql(
    tbl: &SealedTableSnapshot,
    shard_cache: &ShardCache,
) -> CompassResult<String> {
    shard_cache.check_table(&tbl.schema.name)?;
    let explicit_cols = tbl
        .schema
        .columns
        .iter()
        .map(|c| quote_ident(&c.name))
        .collect::<CompassResult<Vec<_>>>()?
        .join(", ");

    if tbl.files.is_empty() {
        // Empty table: synthesize typed zero-row query matching `tbl.schema.columns`.
        let typed_nulls = tbl
            .schema
            .columns
            .iter()
            .map(|c| {
                let dt = sanitize_duckdb_type(&c.data_type)?;
                Ok(format!("NULL::{dt} AS {}", quote_ident(&c.name)?))
            })
            .collect::<CompassResult<Vec<_>>>()?
            .join(", ");
        return Ok(format!("SELECT {typed_nulls} WHERE 1=0"));
    }

    let mut branches = Vec::with_capacity(tbl.files.len());
    for fp in &tbl.files {
        let data_path = shard_cache.shard_path(&fp.data_sha256)?;
        if !shard_cache.has_verified(&fp.data_sha256)? {
            return Err(CompassError::IntegrityViolation(format!(
                "missing cached data shard {}",
                data_path.display()
            )));
        }
        let escaped_data = sql_quote_internal_path(&data_path.to_string_lossy());
        match &fp.delete_sha256 {
            Some(del_sha) => {
                let del_path = shard_cache.shard_path(del_sha)?;
                if !shard_cache.has_verified(del_sha)? {
                    return Err(CompassError::IntegrityViolation(format!(
                        "missing cached delete shard {}",
                        del_path.display()
                    )));
                }
                let escaped_del = sql_quote_internal_path(&del_path.to_string_lossy());
                branches.push(format!(
                    "SELECT {explicit_cols} FROM read_parquet('{escaped_data}', file_row_number = true) \
                     WHERE file_row_number NOT IN (SELECT pos FROM read_parquet('{escaped_del}'))"
                ));
            }
            None => {
                branches.push(format!(
                    "SELECT {explicit_cols} FROM read_parquet('{escaped_data}')"
                ));
            }
        }
    }
    Ok(branches.join(" UNION ALL "))
}

fn build_grant_scoped_source_sql(
    tbl: &SealedTableSnapshot,
    shard_cache: &ShardCache,
    grant: &DatasetGrant,
) -> CompassResult<(String, Vec<DuckValue>)> {
    let base_scan = build_sealed_table_scan_sql(tbl, shard_cache)?;
    let (scope_where, scope_params) = compile_filter_expr(grant.row_scope.as_ref(), &tbl.schema)?;
    Ok((
        format!("SELECT * FROM ({base_scan}) AS raw_tbl WHERE {scope_where}"),
        scope_params,
    ))
}

fn ensure_compatible_schema(primary: &TableSchema, attached: &TableSchema) -> CompassResult<()> {
    if primary.columns.len() != attached.columns.len() {
        return Err(CompassError::Validation(format!(
            "attached table '{}' column count ({}) does not match primary ({})",
            attached.name,
            attached.columns.len(),
            primary.columns.len()
        )));
    }
    for (a, b) in primary.columns.iter().zip(&attached.columns) {
        if a.name != b.name || !a.data_type.eq_ignore_ascii_case(&b.data_type) {
            return Err(CompassError::Validation(format!(
                "attached table '{}' column '{}:{}' incompatible with '{}:{}'",
                attached.name, b.name, b.data_type, a.name, a.data_type
            )));
        }
    }
    Ok(())
}

fn resolve_projected_columns(
    requested: &[String],
    schema: &TableSchema,
) -> CompassResult<Vec<ColumnDescriptor>> {
    if requested.is_empty() {
        return Ok(schema.columns.clone());
    }
    let mut out = Vec::with_capacity(requested.len());
    for col_name in requested {
        let found = find_schema_column(col_name, schema)?;
        out.push(found.clone());
    }
    Ok(out)
}

fn find_schema_column<'a>(name: &str, schema: &'a TableSchema) -> CompassResult<&'a ColumnDescriptor> {
    let trimmed = name.trim();
    validate_sql_ident(trimmed)?;
    schema
        .columns
        .iter()
        .find(|c| c.name == trimmed)
        .ok_or_else(|| {
            CompassError::Validation(format!(
                "unknown column '{trimmed}' on table '{}'",
                schema.name
            ))
        })
}

/// Compile a recursive `FilterExpr` into parameterized DuckDB SQL (`(sql, bind_params)` — zero raw SQL!).
pub fn compile_filter_expr(
    expr: Option<&FilterExpr>,
    schema: &TableSchema,
) -> CompassResult<(String, Vec<DuckValue>)> {
    let Some(root) = expr else {
        return Ok(("1=1".into(), Vec::new()));
    };
    let mut params = Vec::new();
    let sql = compile_filter_node(root, schema, &mut params, 0)?;
    Ok((sql, params))
}

fn compile_filter_node(
    node: &FilterExpr,
    schema: &TableSchema,
    params: &mut Vec<DuckValue>,
    depth: usize,
) -> CompassResult<String> {
    if depth > 32 {
        return Err(CompassError::Validation(
            "FilterExpr nesting depth exceeds maximum of 32".into(),
        ));
    }
    match node {
        FilterExpr::And { exprs } => {
            if exprs.is_empty() {
                return Ok("1=1".into());
            }
            let mut parts = Vec::with_capacity(exprs.len());
            for sub in exprs {
                parts.push(format!("({})", compile_filter_node(sub, schema, params, depth + 1)?));
            }
            Ok(parts.join(" AND "))
        }
        FilterExpr::Or { exprs } => {
            if exprs.is_empty() {
                return Ok("1=0".into());
            }
            let mut parts = Vec::with_capacity(exprs.len());
            for sub in exprs {
                parts.push(format!("({})", compile_filter_node(sub, schema, params, depth + 1)?));
            }
            Ok(parts.join(" OR "))
        }
        FilterExpr::Eq { column, value } => {
            let col = find_schema_column(column, schema)?;
            params.push(scalar_to_duck_value(value)?);
            Ok(format!("{} = ?", quote_ident(&col.name)?))
        }
        FilterExpr::Neq { column, value } => {
            let col = find_schema_column(column, schema)?;
            params.push(scalar_to_duck_value(value)?);
            Ok(format!("{} <> ?", quote_ident(&col.name)?))
        }
        FilterExpr::In { column, values } => {
            let col = find_schema_column(column, schema)?;
            if values.is_empty() {
                return Ok("1=0".into());
            }
            if values.len() > 2_000 {
                return Err(CompassError::Validation(
                    "FilterExpr::In exceeds maximum of 2,000 values".into(),
                ));
            }
            let mut placeholders = Vec::with_capacity(values.len());
            for v in values {
                params.push(scalar_to_duck_value(v)?);
                placeholders.push("?");
            }
            Ok(format!(
                "{} IN ({})",
                quote_ident(&col.name)?,
                placeholders.join(", ")
            ))
        }
        FilterExpr::Lt { column, value } => {
            let col = find_schema_column(column, schema)?;
            params.push(scalar_to_duck_value(value)?);
            Ok(format!("{} < ?", quote_ident(&col.name)?))
        }
        FilterExpr::Lte { column, value } => {
            let col = find_schema_column(column, schema)?;
            params.push(scalar_to_duck_value(value)?);
            Ok(format!("{} <= ?", quote_ident(&col.name)?))
        }
        FilterExpr::Gt { column, value } => {
            let col = find_schema_column(column, schema)?;
            params.push(scalar_to_duck_value(value)?);
            Ok(format!("{} > ?", quote_ident(&col.name)?))
        }
        FilterExpr::Gte { column, value } => {
            let col = find_schema_column(column, schema)?;
            params.push(scalar_to_duck_value(value)?);
            Ok(format!("{} >= ?", quote_ident(&col.name)?))
        }
        FilterExpr::IsNull { column } => {
            let col = find_schema_column(column, schema)?;
            Ok(format!("{} IS NULL", quote_ident(&col.name)?))
        }
        FilterExpr::IsNotNull { column } => {
            let col = find_schema_column(column, schema)?;
            Ok(format!("{} IS NOT NULL", quote_ident(&col.name)?))
        }
        FilterExpr::StartsWith { column, prefix } => {
            let col = find_schema_column(column, schema)?;
            params.push(DuckValue::Text(prefix.clone()));
            Ok(format!(
                "starts_with({}::VARCHAR, ?)",
                quote_ident(&col.name)?
            ))
        }
    }
}

fn compile_order_by(order_by: &[SortSpec], schema: &TableSchema) -> CompassResult<String> {
    if order_by.is_empty() {
        if let Some(pk) = schema.primary_key.first() {
            return Ok(format!("ORDER BY {} ASC", quote_ident(pk)?));
        }
        return Ok(String::new());
    }
    let mut parts = Vec::with_capacity(order_by.len());
    for s in order_by {
        let col = find_schema_column(&s.column, schema)?;
        let dir = match s.direction {
            SortDirection::Asc => "ASC",
            SortDirection::Desc => "DESC",
        };
        parts.push(format!("{} {dir}", quote_ident(&col.name)?));
    }
    Ok(format!("ORDER BY {}", parts.join(", ")))
}

fn scalar_to_duck_value(v: &ScalarValue) -> CompassResult<DuckValue> {
    match v {
        ScalarValue::Bool(b) => Ok(DuckValue::Boolean(*b)),
        ScalarValue::Int(i) => Ok(DuckValue::BigInt(*i)),
        ScalarValue::Float(f) => {
            if !f.is_finite() {
                return Err(CompassError::Validation(
                    "floating-point filter value must be finite".into(),
                ));
            }
            Ok(DuckValue::Double(*f))
        }
        ScalarValue::Text(s) => Ok(DuckValue::Text(s.clone())),
    }
}

fn validate_sha256_hex(raw: &str) -> CompassResult<()> {
    let ok = raw.len() == 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !ok {
        return Err(CompassError::Validation(format!(
            "invalid SHA-256 hex digest '{raw}'"
        )));
    }
    Ok(())
}

fn validate_sql_ident(raw: &str) -> CompassResult<&str> {
    if raw.is_empty() || raw.len() > 128 {
        return Err(CompassError::Validation(format!(
            "SQL identifier '{raw}' must be 1..=128 ASCII chars"
        )));
    }
    let first = raw.as_bytes()[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return Err(CompassError::Validation(format!(
            "SQL identifier '{raw}' must start with [A-Za-z_]"
        )));
    }
    if !raw.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(CompassError::Validation(format!(
            "SQL identifier '{raw}' contains disallowed characters"
        )));
    }
    Ok(raw)
}

fn quote_ident(raw: &str) -> CompassResult<String> {
    let valid = validate_sql_ident(raw)?;
    Ok(format!("\"{valid}\""))
}

fn sanitize_duckdb_type(raw: &str) -> CompassResult<&str> {
    let ok = !raw.is_empty()
        && raw.len() <= 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b' ' | b'(' | b')' | b','));
    if !ok {
        return Err(CompassError::Validation(format!(
            "invalid DuckDB column type '{raw}'"
        )));
    }
    Ok(raw)
}

fn sql_quote_internal_path(raw: &str) -> String {
    raw.replace('\'', "''")
}

fn write_sealed_snapshot_file(catalog_dir: &Path, snap: &SealedSnapshot) -> CompassResult<()> {
    let dir = catalog_dir.join("snapshots");
    std::fs::create_dir_all(&dir)
        .map_err(|e| CompassError::Storage(format!("mkdir {}: {e}", dir.display())))?;
    let path = dir.join(format!("snapshot_{}.json", snap.snapshot_id));
    let json = serde_json::to_string_pretty(snap)
        .map_err(|e| CompassError::Internal(format!("serialize SealedSnapshot: {e}")))?;
    std::fs::write(&path, json)
        .map_err(|e| CompassError::Storage(format!("write {}: {e}", path.display())))
}

fn write_catalog_summary(catalog_dir: &Path, summary: &CatalogSummary) -> CompassResult<()> {
    let path = catalog_dir.join("catalog.json");
    let json = serde_json::to_string_pretty(summary)
        .map_err(|e| CompassError::Internal(format!("serialize catalog.json: {e}")))?;
    std::fs::write(&path, json)
        .map_err(|e| CompassError::Storage(format!("write {}: {e}", path.display())))
}

fn load_sidecar(catalog_dir: &Path) -> CatalogLocalSidecar {
    let p = catalog_dir.join("catalog_state.json");
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_sidecar(catalog_dir: &Path, sidecar: &CatalogLocalSidecar) -> CompassResult<()> {
    let p = catalog_dir.join("catalog_state.json");
    let json = serde_json::to_string_pretty(sidecar)
        .map_err(|e| CompassError::Internal(format!("serialize catalog_state.json: {e}")))?;
    std::fs::write(&p, json)
        .map_err(|e| CompassError::Storage(format!("write {}: {e}", p.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use biocompass_core_traits::OrgId;
    use tempfile::tempdir;

    #[test]
    fn arrow_projection_keeps_nested_and_null_values() {
        use duckdb::arrow::array::NullArray;
        let nulls = NullArray::new(1);
        assert_eq!(arrow_cell_to_json(&nulls, 0, None).unwrap(), Value::Null);
        let con = Connection::open_in_memory().unwrap();
        let mut stmt = con
            .prepare(
                "SELECT ['x', NULL] AS labels, []::VARCHAR[] AS empty_labels,
             NULL::VARCHAR[] AS absent, {'parts': [1, NULL], 'note': NULL::VARCHAR} AS details,
             MAP(['a'], [[2, NULL]]) AS properties, MAP([9], ['nine']) AS numbered,
             DATE '2000-01-02' AS original_date, [DATE '2000-01-02'] AS dates",
            )
            .unwrap();
        let batches: Vec<_> = stmt.query_arrow([]).unwrap().collect();
        let rows = record_batches_to_json_rows(&batches).unwrap();
        let row = &rows[0];
        assert_eq!(row["labels"], serde_json::json!(["x", null]));
        assert_eq!(row["empty_labels"], serde_json::json!([]));
        assert_eq!(row["absent"], Value::Null);
        assert_eq!(
            row["details"],
            serde_json::json!({"parts": [1, null], "note": null})
        );
        assert_eq!(row["properties"], serde_json::json!({"a": [2, null]}));
        assert!(
            matches!(&row["numbered"], Value::Array(items) if items.len() == 1
            && items[0].as_object().unwrap().values().any(|v| v.as_i64() == Some(9))
            && items[0].as_object().unwrap().values().any(|v| v.as_str() == Some("nine")))
        );
        assert_eq!(row["dates"], serde_json::json!([row["original_date"]]));
    }

    #[test]
    fn exact_expiry_retries_only_remaining_claimed_versions() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_duckdb_with_extensions(&["ducklake"]).unwrap();
        conn.execute_batch(&format!(
            "ATTACH 'ducklake:{}' AS cloud_dl (DATA_PATH '{}');
             CREATE TABLE cloud_dl.probe(i INTEGER);",
            dir.path().join("meta.duckdb").display(),
            dir.path().join("data").display(),
        ))
        .unwrap();
        let mut ids = Vec::new();
        for i in 1..=4 {
            conn.execute_batch(&format!("INSERT INTO cloud_dl.probe VALUES ({i})"))
                .unwrap();
            ids.push(current_ducklake_snapshot_id(&conn, "cloud_dl").unwrap());
        }
        expire_claimed_versions(&conn, &BTreeSet::from([ids[0]])).unwrap();
        let claim = BTreeSet::from([ids[0], ids[2]]);
        expire_claimed_versions(&conn, &claim).unwrap();
        expire_claimed_versions(&conn, &claim).unwrap();
        let remaining = conn
            .prepare("SELECT snapshot_id FROM ducklake_snapshots('cloud_dl')")
            .unwrap()
            .query_map([], |row| row.get::<_, u64>(0))
            .unwrap()
            .collect::<Result<BTreeSet<_>, _>>()
            .unwrap();
        assert!(claim.is_disjoint(&remaining));
        assert!(remaining.contains(&ids[1]) && remaining.contains(&ids[3]));
        assert!(expire_claimed_versions(&conn, &BTreeSet::from([ids[3]])).is_err());
    }

    #[test]
    fn lifecycle_append_edit_rollback_pin_expire_and_verify_once_cache() {
        let dir = tempdir().expect("tempdir");
        let summary = seed_demo_hla_catalog(dir.path()).expect("seed DuckLake catalog");
        assert_eq!(summary.row_count, 135);
        assert!(summary.facets.get("hla_alleles").unwrap().contains(&"HLA-A*02:01".to_string()));
        assert!(summary.facets.get("organs").unwrap().contains(&"Lung".to_string()));

        let mut engine = DuckLakeCatalogEngine::open_verified(dir.path()).expect("open verified");
        let s_initial = engine.summary().latest_snapshot_id;
        let digest_initial = engine.summary().dataset_digest.clone();

        // Pin initial snapshot so expiry retains it
        engine.pin_snapshot(s_initial).expect("pin initial");

        // Append 1 new row -> new sealed snapshot
        let s_append = engine
            .commit_mutation("Append PEP-9001", |conn| {
                conn.execute_batch(
                    "INSERT INTO dl.peptides VALUES ('PEP-9001', 'GILGFVFTL', 9, 'HLA-A*02:01', 'FLU_M1', 'Lung', 'NSCLC_Tumor', 6.4, 0.05, 25.0, 14);",
                )
                .map_err(|e| CompassError::Storage(e.to_string()))
            })
            .expect("append row");
        assert_eq!(s_append.total_row_count(), 136);
        assert_ne!(s_append.dataset_digest, digest_initial);

        // Edit + delete -> new sealed snapshot
        let s_edit = engine
            .commit_mutation("Update PEP-9001 & delete PEP-0001", |conn| {
                conn.execute_batch(
                    "UPDATE dl.peptides SET affinity_nm = 4.2 WHERE peptide_id = 'PEP-9001';
                     DELETE FROM dl.peptides WHERE peptide_id = 'PEP-0001';",
                )
                .map_err(|e| CompassError::Storage(e.to_string()))
            })
            .expect("edit & delete");
        assert_eq!(s_edit.total_row_count(), 135);

        let full_grant = DatasetGrant::full_access(OrgId::from("org-owner"), summary.id.clone());

        // Time-travel query at pinned initial snapshot still has PEP-0001 and 135 rows
        let res_initial = engine
            .query(
                &full_grant,
                &QueryAst {
                    snapshot_id: Some(s_initial),
                    filter: Some(FilterExpr::eq("peptide_id", "PEP-0001")),
                    ..Default::default()
                },
            )
            .expect("query pinned initial");
        assert_eq!(res_initial.rows.len(), 1);
        assert_eq!(res_initial.rows[0]["sequence"], "KLFGTSGQKT");

        // Expire unpinned historical snapshots keeping only latest 1 -> s_append expires, pinned s_initial survives!
        let expired = engine
            .expire_unpinned_snapshots(1)
            .expect("expire unpinned");
        assert!(expired.contains(&s_append.snapshot_id));
        assert!(!expired.contains(&s_initial));
        assert!(engine.load_sealed_snapshot(Some(s_append.snapshot_id)).is_err());
        assert_eq!(
            engine
                .query(
                    &full_grant,
                    &QueryAst {
                        snapshot_id: Some(s_initial),
                        ..Default::default()
                    }
                )
                .expect("pinned snapshot still readable")
                .total_in_scope,
            135
        );

        // Rollback to `s_initial` creates a forward commit restoring PEP-0001 and removing PEP-9001!
        let rolled = engine
            .rollback_to_snapshot(s_initial)
            .expect("rollback to s_initial");
        assert!(rolled.snapshot_id > s_edit.snapshot_id);
        let check_pep9001 = engine
            .query(
                &full_grant,
                &QueryAst {
                    filter: Some(FilterExpr::eq("peptide_id", "PEP-9001")),
                    ..Default::default()
                },
            )
            .expect("query after rollback");
        assert_eq!(check_pep9001.total_in_scope, 0);

        // B12: Verify-once ShardCache never re-hashes across repeated queries
        let ingests_before = engine.shard_cache().verified_ingest_count();
        for _ in 0..5 {
            let _ = engine.query(&full_grant, &QueryAst::default()).unwrap();
        }
        assert_eq!(
            engine.shard_cache().verified_ingest_count(),
            ingests_before,
            "B12: queries must not re-hash cached shards"
        );
    }

    #[test]
    fn compute_only_refuses_queries_and_custom_staging_while_b11_fails_loud_on_overflow() {
        let dir = tempdir().expect("tempdir");
        let summary = seed_demo_hla_catalog(dir.path()).expect("seed");
        let engine = DuckLakeCatalogEngine::open_verified(dir.path()).expect("open");

        let compute_only =
            DatasetGrant::compute_only_partner(OrgId::from("org-partner"), summary.id.clone());
        // Direct row query refused on compute_only
        assert!(matches!(
            engine.query(&compute_only, &QueryAst::default()),
            Err(CompassError::AccessDenied(_))
        ));
        // Custom module staging refused on compute_only
        let stage_path = dir.path().join("custom_stage.parquet");
        assert!(matches!(
            engine.materialize_parquet_for_run(
                &compute_only,
                &QueryAst::default(),
                StagingMode::CustomerScoped,
                &stage_path
            ),
            Err(CompassError::AccessDenied(_))
        ));
        // Curated full-scan module staging SUCCEEDS on compute_only with all 135 unmasked rows!
        let curated_slice = engine
            .materialize_parquet_for_run(
                &compute_only,
                &QueryAst::default(),
                StagingMode::CuratedFullScan,
                &stage_path,
            )
            .expect("curated full scan on compute_only");
        assert_eq!(curated_slice.row_count, 135);

        // B11: CustomerScoped with max_rows_per_query = 20 FAILS LOUDLY when 135 rows match (zero silent truncation!)
        let mut small_rows_grant =
            DatasetGrant::full_access(OrgId::from("org-partner"), summary.id);
        small_rows_grant.max_rows_per_query = 20;
        assert!(matches!(
            engine.materialize_parquet_for_run(
                &small_rows_grant,
                &QueryAst::default(),
                StagingMode::CustomerScoped,
                &dir.path().join("overflow.parquet")
            ),
            Err(CompassError::AccessDenied(_))
        ));
    }

    #[test]
    fn same_plane_multi_catalog_attach_and_hospital_readonly() {
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();
        let sum_a = seed_demo_hla_catalog(dir_a.path()).unwrap();

        let mut pks = BTreeMap::new();
        pks.insert("peptides".into(), vec!["peptide_id".into()]);
        let eng_b = DuckLakeCatalogEngine::create_local_catalog(
            dir_b.path(),
            CatalogId::from("alithea/hla-extra-v1"),
            "Extra Cohort",
            "1.0.0",
            "Secondary same-plane cohort",
            "Homo sapiens",
            pks,
            "Init extra",
            |conn| {
                conn.execute_batch(
                    "CREATE TABLE dl.peptides (
                        peptide_id VARCHAR NOT NULL,
                        sequence VARCHAR NOT NULL,
                        peptide_length INTEGER NOT NULL,
                        hla_allele VARCHAR NOT NULL,
                        gene_symbol VARCHAR NOT NULL,
                        organ VARCHAR NOT NULL,
                        condition VARCHAR NOT NULL,
                        affinity_nm DOUBLE NOT NULL,
                        eluted_rank_pct DOUBLE NOT NULL,
                        ms_intensity_log2 DOUBLE NOT NULL,
                        donor_count INTEGER NOT NULL
                    );
                    INSERT INTO dl.peptides VALUES
                        ('EXTRA-1', 'NLVPMVATV', 9, 'HLA-A*02:01', 'CMV_PP65', 'Lung', 'NSCLC_Tumor', 8.1, 0.08, 24.5, 19);",
                )
                .map_err(|e| CompassError::Storage(e.to_string()))
            },
        )
        .unwrap();

        let eng_a = DuckLakeCatalogEngine::open_verified(dir_a.path()).unwrap();
        let grant_a = DatasetGrant::full_access(OrgId::from("org-owner"), sum_a.id);
        let grant_b = DatasetGrant::full_access(OrgId::from("org-owner"), eng_b.summary().id.clone());

        let combined = eng_a
            .query_multi(
                &grant_a,
                &QueryAst {
                    limit: Some(500),
                    ..Default::default()
                },
                &[(&eng_b, &grant_b, None)],
            )
            .expect("multi-catalog query");
        assert_eq!(combined.total_in_scope, 136);
        let page = QueryAst {
            columns: vec!["peptide_id".into()],
            order_by: vec![biocompass_core_traits::SortSpec {
                column: "peptide_id".into(),
                direction: Default::default(),
            }],
            limit: Some(2),
            ..Default::default()
        };
        let (page_result, counts) = eng_a
            .query_multi_with_row_counts(&grant_a, &page, &[(&eng_b, &grant_b, None)])
            .unwrap();
        assert_eq!(page_result.rows[0]["peptide_id"], "EXTRA-1");
        assert_eq!(page_result.rows[1]["peptide_id"], "PEP-0001");
        assert!(page_result.rows.iter().all(|row| row.len() == 1));
        assert_eq!(counts, BTreeMap::from([(grant_a.grant_id.clone(), 1), (grant_b.grant_id.clone(), 1)]));
        let (_, duplicate_counts) = eng_a
            .query_multi_with_row_counts(&grant_a, &page, &[(&eng_a, &grant_a, None)])
            .unwrap();
        assert_eq!(duplicate_counts, BTreeMap::from([(grant_a.grant_id.clone(), 2)]));

        // Hospital read-only mode rejects mutations
        let mut hosp = DuckLakeCatalogEngine::open_hospital_readonly(dir_a.path()).unwrap();
        assert!(hosp.is_read_only());
        assert!(matches!(
            hosp.commit_mutation("illegal write", |_| Ok(())),
            Err(CompassError::AccessDenied(_))
        ));
    }

    #[test]
    fn quarantine_append_casts_null_only_parquet_columns_without_promoting_them() {
        let work = tempdir().unwrap();
        let input = work.path().join("in");
        let output = work.path().join("out");
        let source = input.join("quarantine/rows.parquet");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        let expected = TableSchema {
            name: "samples".into(),
            columns: vec![
                ColumnDescriptor {
                    name: "id".into(),
                    data_type: "BIGINT".into(),
                    nullable: true,
                },
                ColumnDescriptor {
                    name: "subtype".into(),
                    data_type: r#""NULL""#.into(),
                    nullable: true,
                },
                ColumnDescriptor {
                    name: "aliases".into(),
                    data_type: r#""NULL"[]"#.into(),
                    nullable: true,
                },
            ],
            primary_key: vec!["id".into()],
            row_count: 1,
        };
        let spec = QuarantineIngestSpec {
            filename: "rows.parquet".into(),
            format: IngestFormat::Parquet,
            mode: IngestMode::Append,
            table_name: "samples".into(),
            primary_key: expected.primary_key.clone(),
            expected_schema: Some(expected),
        };
        std::fs::write(
            input.join("ingest_spec.json"),
            serde_json::to_vec(&spec).unwrap(),
        )
        .unwrap();
        let conn = open_parquet_only_duckdb().unwrap();
        for (subtype, aliases, accepted) in [
            ("NULL::VARCHAR", "[NULL]::VARCHAR[]", true),
            ("'new'::VARCHAR", "[NULL]::VARCHAR[]", false),
            ("NULL::VARCHAR", "['new']::VARCHAR[]", false),
        ] {
            conn.execute_batch(&format!(
                "COPY (SELECT 2::BIGINT AS id, {subtype} AS subtype, {aliases} AS aliases) TO '{}' (FORMAT PARQUET)",
                sql_quote_internal_path(&source.to_string_lossy())
            )).unwrap();
            let result = validate_and_normalize_quarantine_dir(&input, &output);
            if accepted {
                let report = result.unwrap();
                assert_eq!(report.row_count, 1);
                let nullable = conn.query_row(
                    &format!("SELECT subtype IS NULL AND list_count(aliases) = 0 FROM read_parquet('{}')",
                        sql_quote_internal_path(&output.join("normalized.parquet").to_string_lossy())),
                    [], |row| row.get::<_, bool>(0)
                ).unwrap();
                assert!(nullable);
            } else {
                assert!(
                    matches!(result, Err(CompassError::Validation(ref msg)) if msg.contains("type mismatch"))
                );
            }
        }
    }

    #[test]
    fn saved_view_selection_digest_and_quarantine_ingest_modes() {
        let work = tempdir().unwrap();
        let cat_dir = work.path().join("cat");

        // 1. Validate & normalize CSV quarantine upload (Replace -> creates initial catalog)
        let in_v1 = work.path().join("in_v1");
        let out_v1 = work.path().join("out_v1");
        std::fs::create_dir_all(in_v1.join("quarantine")).unwrap();
        std::fs::write(
            in_v1.join("quarantine/genes.csv"),
            "gene_id,symbol,score\nG1,TP53,9.5\nG2,EGFR,7.2\nG3,KRAS,8.8\n",
        )
        .unwrap();
        let spec_v1 = QuarantineIngestSpec {
            filename: "genes.csv".into(),
            table_name: "genes".into(),
            format: IngestFormat::Csv,
            mode: IngestMode::Replace,
            primary_key: vec!["gene_id".into()],
            expected_schema: None,
        };
        std::fs::write(
            in_v1.join("ingest_spec.json"),
            serde_json::to_vec(&spec_v1).unwrap(),
        )
        .unwrap();

        let rep_v1 = validate_and_normalize_quarantine_dir(&in_v1, &out_v1).unwrap();
        assert_eq!(rep_v1.row_count, 3);
        assert_eq!(rep_v1.primary_key, vec!["gene_id".to_string()]);

        // Reject duplicate PK in quarantine validator
        let in_dup = work.path().join("in_dup");
        let out_dup = work.path().join("out_dup");
        std::fs::create_dir_all(in_dup.join("quarantine")).unwrap();
        std::fs::write(
            in_dup.join("quarantine/genes.csv"),
            "gene_id,symbol,score\nG1,TP53,9.5\nG1,TP53_DUP,1.0\n",
        )
        .unwrap();
        std::fs::write(
            in_dup.join("ingest_spec.json"),
            serde_json::to_vec(&spec_v1).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            validate_and_normalize_quarantine_dir(&in_dup, &out_dup),
            Err(CompassError::Validation(_))
        ));

        // 2. Import normalized Parquet into local DuckLake catalog
        let pk_cols = vec!["gene_id".to_string()];
        let mut eng = DuckLakeCatalogEngine::create_or_import_local_parquet(
            &cat_dir,
            CatalogId::from("org/genes-v1"),
            "Gene Scores",
            "Target gene scores",
            "Homo sapiens",
            "genes",
            IngestMode::Replace,
            &pk_cols,
            &out_v1.join("normalized.parquet"),
            "Initial CSV import",
        )
        .unwrap();
        let snap1_id = eng.summary().latest_snapshot_id;

        let full_grant = DatasetGrant::full_access(OrgId::from("org-1"), eng.summary().id.clone());
        let ast_pinned = QueryAst {
            table: Some("genes".into()),
            snapshot_id: Some(snap1_id),
            limit: Some(1), // Pagination MUST NOT truncate Saved View PK selection_digest!
            ..Default::default()
        };
        let sel1 = eng
            .compute_selection_digest(&full_grant, &ast_pinned)
            .unwrap();
        assert_eq!(sel1.row_count, 3);

        // Tightened row_scope changes selection_digest -> AccessChanged
        let mut scoped_grant = full_grant.clone();
        scoped_grant.row_scope = Some(FilterExpr::Gt {
            column: "score".into(),
            value: ScalarValue::Float(8.0),
        });
        let sel_scoped = eng
            .compute_selection_digest(&scoped_grant, &ast_pinned)
            .unwrap();
        assert_eq!(sel_scoped.row_count, 2);
        assert_ne!(sel_scoped.selection_digest, sel1.selection_digest);

        // 3. Append & Edit mutations advance head selection_digest (DataChanged) while pinned snapshot stays identical
        let in_edit = work.path().join("in_edit");
        let out_edit = work.path().join("out_edit");
        std::fs::create_dir_all(in_edit.join("quarantine")).unwrap();
        std::fs::write(
            in_edit.join("quarantine/patch.tsv"),
            "gene_id\tsymbol\tscore\nG2\tEGFR\t9.9\nG4\tMYC\t8.4\n",
        )
        .unwrap();
        let existing_schema = eng
            .load_sealed_snapshot(None)
            .unwrap()
            .resolve_table(Some("genes"))
            .unwrap()
            .schema
            .clone();
        let spec_edit = QuarantineIngestSpec {
            filename: "patch.tsv".into(),
            table_name: "genes".into(),
            format: IngestFormat::Tsv,
            mode: IngestMode::Edit,
            primary_key: pk_cols.clone(),
            expected_schema: Some(existing_schema),
        };
        std::fs::write(
            in_edit.join("ingest_spec.json"),
            serde_json::to_vec(&spec_edit).unwrap(),
        )
        .unwrap();
        let rep_edit = validate_and_normalize_quarantine_dir(&in_edit, &out_edit).unwrap();
        assert_eq!(rep_edit.row_count, 2);

        let snap2 = eng
            .import_normalized_parquet(
                "genes",
                IngestMode::Edit,
                &pk_cols,
                &out_edit.join("normalized.parquet"),
                "TSV upsert edit",
            )
            .unwrap();
        assert!(snap2.snapshot_id > snap1_id);

        // Pinned snap1 still matches sel1; head (None) has 4 rows and different digest
        let sel_pinned = eng
            .compute_selection_digest(&full_grant, &ast_pinned)
            .unwrap();
        assert_eq!(sel_pinned.selection_digest, sel1.selection_digest);

        let ast_head = QueryAst {
            table: Some("genes".into()),
            snapshot_id: None,
            ..Default::default()
        };
        let sel_head = eng
            .compute_selection_digest(&full_grant, &ast_head)
            .unwrap();
        assert_eq!(sel_head.row_count, 4);
        assert_ne!(sel_head.selection_digest, sel1.selection_digest);

        // 4. Table without declared PK is rejected for Saved Views (§7.1)
        let in_nopk = work.path().join("in_nopk");
        let out_nopk = work.path().join("out_nopk");
        std::fs::create_dir_all(in_nopk.join("quarantine")).unwrap();
        std::fs::write(in_nopk.join("quarantine/notes.csv"), "note\nhello\n").unwrap();
        std::fs::write(
            in_nopk.join("ingest_spec.json"),
            serde_json::to_vec(&QuarantineIngestSpec {
                filename: "notes.csv".into(),
                table_name: "notes".into(),
                format: IngestFormat::Csv,
                mode: IngestMode::Replace,
                primary_key: Vec::new(),
                expected_schema: None,
            })
            .unwrap(),
        )
        .unwrap();
        validate_and_normalize_quarantine_dir(&in_nopk, &out_nopk).unwrap();
        eng.import_normalized_parquet(
            "notes",
            IngestMode::Replace,
            &[],
            &out_nopk.join("normalized.parquet"),
            "No-PK table",
        )
        .unwrap();
        assert!(matches!(
            eng.compute_selection_digest(
                &full_grant,
                &QueryAst {
                    table: Some("notes".into()),
                    ..Default::default()
                },
            ),
            Err(CompassError::Validation(_))
        ));
    }

    #[test]
    fn dev_07_duckdb_extension_manifest_and_bootstrap_script() {
        let ws_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root")
            .to_path_buf();
        let manifest_path = ws_root.join("vendor/duckdb-extensions/v2.0.0.sha256");
        assert!(
            manifest_path.is_file(),
            "expected tracked manifest at {}",
            manifest_path.display()
        );
        let manifest = std::fs::read_to_string(&manifest_path).unwrap();
        let mut entries = std::collections::BTreeSet::new();
        for line in manifest.lines().filter(|l| !l.trim().is_empty()) {
            let mut parts = line.split_whitespace();
            let digest = parts.next().expect("sha256 digest");
            let rel_file = parts.next().expect("relative extension path");
            assert!(parts.next().is_none(), "unexpected extra column in {line}");
            assert_eq!(digest.len(), 64, "invalid sha256 length in {line}");
            assert!(
                digest.chars().all(|c| c.is_ascii_hexdigit()),
                "non-hex sha256 in {line}"
            );
            entries.insert(rel_file.to_string());
        }
        for platform in ["linux_amd64", "linux_arm64", "osx_arm64"] {
            for ext in ["ducklake", "httpfs", "parquet", "postgres_scanner"] {
                let expected = format!("{platform}/{ext}.duckdb_extension");
                assert!(
                    entries.contains(&expected),
                    "manifest missing {expected}"
                );
            }
        }
        assert_eq!(entries.len(), 12);

        let script_path = ws_root.join("scripts/install-duckdb-extensions.sh");
        assert!(
            script_path.is_file(),
            "expected bootstrap script at {}",
            script_path.display()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&script_path).unwrap().permissions().mode();
            assert_ne!(mode & 0o111, 0, "bootstrap script must be executable");
        }

        // Verify idempotent check on host extensions when present, and corruption rejection via --verify-only.
        let status = std::process::Command::new(&script_path)
            .arg("--verify-only")
            .current_dir(&ws_root)
            .status()
            .expect("run install-duckdb-extensions.sh --verify-only");
        assert!(status.success(), "expected host extensions to verify cleanly");

        let bad_dir = tempfile::tempdir().unwrap();
        let bad_plat = bad_dir.path().join("v2.0.0/osx_arm64");
        std::fs::create_dir_all(&bad_plat).unwrap();
        for ext in ["ducklake", "httpfs", "parquet", "postgres_scanner"] {
            std::fs::write(bad_plat.join(format!("{ext}.duckdb_extension")), b"corrupt").unwrap();
        }
        let bad_status = std::process::Command::new(&script_path)
            .args(["--verify-only", "osx_arm64"])
            .arg(bad_dir.path())
            .current_dir(&ws_root)
            .status()
            .expect("run install-duckdb-extensions.sh --verify-only on corrupt dir");
        assert!(
            !bad_status.success(),
            "corrupt extension directory must fail sha256 verification"
        );
    }
}
