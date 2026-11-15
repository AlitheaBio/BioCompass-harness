//! `biocompass-data-engine`: Embedded DuckDB query & slice materialization engine over immutable
//! Parquet folders with `catalog.json` SHA-256 verification and Grant row/`***` sequence masking.

pub mod seed;

use biocompass_core_traits::{CatalogId, CompassError, CompassResult, DatasetGrant};
use duckdb::types::Value as DuckValue;
use duckdb::{Connection, params_from_iter};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub use seed::seed_demo_hla_catalog;

/// Column metadata declared in `catalog.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogColumnSpec {
    pub name: String,
    pub sql_type: String,
    /// Indicates whether `DatasetGrant::mask_peptide_sequences` masks this column with `'***'`.
    pub maskable: bool,
}

/// Immutable Parquet file shard entry pinned by SHA-256 in `catalog.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParquetShardEntry {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub row_count: u64,
}

/// Immutable dataset manifest (`catalog.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogManifest {
    pub id: CatalogId,
    pub name: String,
    pub version: String,
    pub description: String,
    pub organism: String,
    pub row_count: u64,
    pub columns: Vec<CatalogColumnSpec>,
    pub parquet_files: Vec<ParquetShardEntry>,
}

impl CatalogManifest {
    /// Deterministic SHA-256 digest of the canonical shard digests for Froglet workload binding.
    pub fn dataset_digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.id.as_str().as_bytes());
        hasher.update(b":");
        hasher.update(self.version.as_bytes());
        for shard in &self.parquet_files {
            hasher.update(b":");
            hasher.update(shard.sha256.as_bytes());
        }
        hex::encode(hasher.finalize())
    }
}

/// Structured query parameters compiled into DuckDB bound `?` parameters (zero raw user SQL).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CatalogQuery {
    #[serde(default)]
    pub organs: Vec<String>,
    #[serde(default)]
    pub conditions: Vec<String>,
    #[serde(default)]
    pub hla_alleles: Vec<String>,
    #[serde(default)]
    pub gene_symbols: Vec<String>,
    pub min_length: Option<i32>,
    pub max_length: Option<i32>,
    pub max_affinity_nm: Option<f64>,
    /// Allowed ONLY when `grant.mask_peptide_sequences == false`.
    pub sequence_prefix: Option<String>,
    pub limit: Option<usize>,
}

/// Returned immunopeptidomics row after Grant policy enforcement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeptideRecord {
    pub peptide_id: String,
    pub sequence: String,
    pub peptide_length: i32,
    pub hla_allele: String,
    pub gene_symbol: String,
    pub organ: String,
    pub condition: String,
    pub affinity_nm: f64,
    pub eluted_rank_pct: f64,
    pub ms_intensity_log2: f64,
    pub donor_count: i32,
}

/// Query response carrying rows, total matched count in grant scope, and active SQL policy summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogQueryResult {
    pub catalog_id: CatalogId,
    pub grant_id: String,
    pub sequence_masked: bool,
    pub compiled_view_sql: String,
    pub total_in_scope: usize,
    pub rows: Vec<PeptideRecord>,
}

/// Summary of a pre-masked Parquet slice materialized into `/in/data/cohort.parquet`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterializedSlice {
    pub path: PathBuf,
    pub row_count: u64,
    pub sha256: String,
    pub sequence_masked: bool,
}

/// Embedded DuckDB catalog engine enforcing `catalog.json` SHA-256 digests and `DatasetGrant` views.
#[derive(Debug, Clone)]
pub struct DuckDbCatalogEngine {
    catalog_dir: PathBuf,
    manifest: CatalogManifest,
}

impl DuckDbCatalogEngine {
    /// Load `catalog.json` from `catalog_dir` and verify every Parquet shard's SHA-256 digest.
    pub fn open_verified(catalog_dir: impl AsRef<Path>) -> CompassResult<Self> {
        let dir = catalog_dir.as_ref().to_path_buf();
        let manifest_path = dir.join("catalog.json");
        let raw = std::fs::read_to_string(&manifest_path).map_err(|e| {
            CompassError::NotFound(format!("read {}: {e}", manifest_path.display()))
        })?;
        let manifest: CatalogManifest = serde_json::from_str(&raw)
            .map_err(|e| CompassError::Validation(format!("decode catalog.json: {e}")))?;

        let engine = Self {
            catalog_dir: dir,
            manifest,
        };
        engine.verify_integrity()?;
        Ok(engine)
    }

    /// Borrow the loaded `CatalogManifest`.
    pub const fn manifest(&self) -> &CatalogManifest {
        &self.manifest
    }

    /// Verify every Parquet shard's SHA-256 against `catalog.json`.
    pub fn verify_integrity(&self) -> CompassResult<()> {
        if self.manifest.parquet_files.is_empty() {
            return Err(CompassError::Validation(
                "catalog.json must declare at least one parquet shard".into(),
            ));
        }
        for shard in &self.manifest.parquet_files {
            if shard.relative_path.contains("..") || shard.relative_path.starts_with('/') {
                return Err(CompassError::Validation(format!(
                    "unsafe parquet relative_path '{}'",
                    shard.relative_path
                )));
            }
            let full = self.catalog_dir.join(&shard.relative_path);
            let bytes = std::fs::read(&full).map_err(|e| {
                CompassError::IntegrityViolation(format!(
                    "missing parquet shard {}: {e}",
                    full.display()
                ))
            })?;
            let actual = hex::encode(Sha256::digest(&bytes));
            if actual != shard.sha256 {
                return Err(CompassError::IntegrityViolation(format!(
                    "SHA-256 mismatch on {}: expected {}, computed {}",
                    shard.relative_path, shard.sha256, actual
                )));
            }
        }
        Ok(())
    }

    /// Compile a `DatasetGrant` into a DuckDB `CREATE TEMP VIEW` statement over verified Parquet shards.
    pub fn build_scoped_view_sql(&self, grant: &DatasetGrant) -> CompassResult<String> {
        if grant.catalog_id != self.manifest.id {
            return Err(CompassError::AccessDenied(format!(
                "grant targets catalog '{}', but engine holds '{}'",
                grant.catalog_id, self.manifest.id
            )));
        }

        let shard_list = self
            .manifest
            .parquet_files
            .iter()
            .map(|s| {
                let p = self.catalog_dir.join(&s.relative_path);
                format!("'{}'", sql_quote_internal_path(&p.to_string_lossy()))
            })
            .collect::<Vec<_>>()
            .join(", ");

        let seq_projection = if grant.mask_peptide_sequences {
            "'***'::VARCHAR AS sequence"
        } else {
            "sequence"
        };

        let mut predicates = vec!["1=1".to_string()];
        if let Some(organs) = &grant.allowed_organs {
            if organs.is_empty() {
                predicates.push("1=0".into());
            } else {
                predicates.push(format!("organ IN ({})", quote_grant_tokens(organs)?));
            }
        }
        if let Some(conds) = &grant.allowed_conditions {
            if conds.is_empty() {
                predicates.push("1=0".into());
            } else {
                predicates.push(format!("condition IN ({})", quote_grant_tokens(conds)?));
            }
        }

        Ok(format!(
            "CREATE TEMP VIEW scoped_cohort AS \
             SELECT peptide_id, {seq_projection}, peptide_length, hla_allele, gene_symbol, \
                    organ, condition, affinity_nm, eluted_rank_pct, ms_intensity_log2, donor_count \
             FROM read_parquet([{shard_list}]) \
             WHERE {}",
            predicates.join(" AND ")
        ))
    }

    fn open_scoped_connection(&self, grant: &DatasetGrant) -> CompassResult<(Connection, String)> {
        self.verify_integrity()?;
        let view_sql = self.build_scoped_view_sql(grant)?;
        let conn = Connection::open_in_memory()
            .map_err(|e| CompassError::Storage(format!("open in-memory duckdb: {e}")))?;
        conn.execute_batch(&view_sql).map_err(|e| {
            CompassError::Storage(format!("compile DuckDB scoped_cohort view: {e}"))
        })?;
        Ok((conn, view_sql))
    }

    /// Execute a structured query against the Grant-scoped DuckDB view using prepared `?` bindings.
    pub fn query(
        &self,
        grant: &DatasetGrant,
        q: &CatalogQuery,
    ) -> CompassResult<CatalogQueryResult> {
        enforce_no_masked_oracle(grant, q)?;

        let (conn, view_sql) = self.open_scoped_connection(grant)?;
        let (where_sql, bind_values) = build_bound_predicates(q)?;

        let total_in_scope: usize = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM scoped_cohort WHERE {where_sql}"),
                params_from_iter(bind_values.iter()),
                |r| r.get::<_, i64>(0),
            )
            .map_err(|e| CompassError::Storage(format!("count scoped_cohort: {e}")))?
            as usize;

        let effective_limit = q
            .limit
            .unwrap_or(100)
            .min(grant.max_rows)
            .min(1_000) as i64;

        let mut row_params = bind_values;
        row_params.push(DuckValue::BigInt(effective_limit));

        let sql = format!(
            "SELECT peptide_id, sequence, peptide_length, hla_allele, gene_symbol, \
                    organ, condition, affinity_nm, eluted_rank_pct, ms_intensity_log2, donor_count \
             FROM scoped_cohort \
             WHERE {where_sql} \
             ORDER BY affinity_nm ASC, peptide_id ASC \
             LIMIT ?"
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| CompassError::Storage(format!("prepare catalog query: {e}")))?;
        let mapped = stmt
            .query_map(params_from_iter(row_params.iter()), |r| {
                Ok(PeptideRecord {
                    peptide_id: r.get(0)?,
                    sequence: r.get(1)?,
                    peptide_length: r.get(2)?,
                    hla_allele: r.get(3)?,
                    gene_symbol: r.get(4)?,
                    organ: r.get(5)?,
                    condition: r.get(6)?,
                    affinity_nm: r.get(7)?,
                    eluted_rank_pct: r.get(8)?,
                    ms_intensity_log2: r.get(9)?,
                    donor_count: r.get(10)?,
                })
            })
            .map_err(|e| CompassError::Storage(format!("run catalog query: {e}")))?;

        let mut rows = Vec::new();
        for item in mapped {
            rows.push(
                item.map_err(|e| CompassError::Storage(format!("decode peptide row: {e}")))?,
            );
        }

        Ok(CatalogQueryResult {
            catalog_id: self.manifest.id.clone(),
            grant_id: grant.grant_id.clone(),
            sequence_masked: grant.mask_peptide_sequences,
            compiled_view_sql: view_sql,
            total_in_scope,
            rows,
        })
    }

    /// Materialize a Grant-enforced, pre-masked Parquet slice directly into `dest_parquet`
    /// (e.g. `/in/data/cohort.parquet`) using bound query parameters.
    pub fn materialize_scoped_parquet(
        &self,
        grant: &DatasetGrant,
        q: &CatalogQuery,
        dest_parquet: &Path,
    ) -> CompassResult<MaterializedSlice> {
        enforce_no_masked_oracle(grant, q)?;

        if let Some(parent) = dest_parquet.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CompassError::Storage(format!("create slice dir: {e}")))?;
        }
        let (conn, _) = self.open_scoped_connection(grant)?;
        let (where_sql, mut bind_values) = build_bound_predicates(q)?;
        bind_values.push(DuckValue::BigInt(grant.max_rows as i64));

        // Populate a temporary table using strict prepared `?` bindings, then export via ZSTD Parquet COPY.
        conn.execute(
            &format!(
                "CREATE TEMP TABLE materialized_stage AS \
                 SELECT * FROM scoped_cohort WHERE {where_sql} ORDER BY peptide_id LIMIT ?"
            ),
            params_from_iter(bind_values.iter()),
        )
        .map_err(|e| CompassError::Storage(format!("stage materialized_stage table: {e}")))?;

        let escaped_dest = sql_quote_internal_path(&dest_parquet.to_string_lossy());
        conn.execute_batch(&format!(
            "COPY materialized_stage TO '{escaped_dest}' (FORMAT PARQUET, COMPRESSION ZSTD);"
        ))
        .map_err(|e| CompassError::Storage(format!("export materialized parquet: {e}")))?;

        let row_count: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM read_parquet('{escaped_dest}')"),
                [],
                |r| r.get(0),
            )
            .map_err(|e| CompassError::Storage(format!("count materialized parquet: {e}")))?;

        let bytes = std::fs::read(dest_parquet)
            .map_err(|e| CompassError::Storage(format!("hash materialized parquet: {e}")))?;
        let sha256 = hex::encode(Sha256::digest(&bytes));

        Ok(MaterializedSlice {
            path: dest_parquet.to_path_buf(),
            row_count: row_count as u64,
            sha256,
            sequence_masked: grant.mask_peptide_sequences,
        })
    }
}

fn enforce_no_masked_oracle(grant: &DatasetGrant, q: &CatalogQuery) -> CompassResult<()> {
    if grant.mask_peptide_sequences
        && q.sequence_prefix
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
    {
        return Err(CompassError::AccessDenied(
            "sequence_prefix filtering is forbidden when grant.mask_peptide_sequences is active"
                .into(),
        ));
    }
    Ok(())
}

fn sql_quote_internal_path(raw: &str) -> String {
    raw.replace('\'', "''")
}

/// Validate server-minted `DatasetGrant` allowlist tokens (strict identifier charset).
fn quote_grant_tokens(items: &[String]) -> CompassResult<String> {
    let mut out = Vec::with_capacity(items.len());
    for token in items {
        let trimmed = token.trim();
        if trimmed.is_empty() || trimmed.len() > 64 {
            return Err(CompassError::Validation(format!(
                "invalid grant allowlist token '{token}'"
            )));
        }
        let valid = trimmed.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'*' | b':' | b'.' | b' ')
        });
        if !valid {
            return Err(CompassError::Validation(format!(
                "disallowed character in grant allowlist token '{token}'"
            )));
        }
        out.push(format!("'{}'", sql_quote_internal_path(trimmed)));
    }
    Ok(out.join(", "))
}

/// Build parameterized SQL `WHERE` predicates returning `(sql_fragment, bound_values)`.
fn build_bound_predicates(q: &CatalogQuery) -> CompassResult<(String, Vec<DuckValue>)> {
    let mut clauses = vec!["1=1".to_string()];
    let mut params = Vec::new();

    append_in_clause("organ", &q.organs, &mut clauses, &mut params);
    append_in_clause("condition", &q.conditions, &mut clauses, &mut params);
    append_in_clause("hla_allele", &q.hla_alleles, &mut clauses, &mut params);
    append_in_clause("gene_symbol", &q.gene_symbols, &mut clauses, &mut params);

    if let Some(min_l) = q.min_length {
        clauses.push("peptide_length >= ?".into());
        params.push(DuckValue::Int(min_l));
    }
    if let Some(max_l) = q.max_length {
        clauses.push("peptide_length <= ?".into());
        params.push(DuckValue::Int(max_l));
    }
    if let Some(max_aff) = q.max_affinity_nm {
        if !max_aff.is_finite() {
            return Err(CompassError::Validation(
                "max_affinity_nm must be a finite number".into(),
            ));
        }
        clauses.push("affinity_nm <= ?".into());
        params.push(DuckValue::Double(max_aff));
    }
    if let Some(prefix) = &q.sequence_prefix {
        let trimmed = prefix.trim().to_ascii_uppercase();
        if !trimmed.is_empty() {
            if !trimmed.bytes().all(|b| b.is_ascii_uppercase()) {
                return Err(CompassError::Validation(
                    "sequence_prefix must contain only IUPAC amino-acid letters A-Z".into(),
                ));
            }
            clauses.push("starts_with(sequence, ?)".into());
            params.push(DuckValue::Text(trimmed));
        }
    }

    Ok((clauses.join(" AND "), params))
}

fn append_in_clause(
    column: &str,
    items: &[String],
    clauses: &mut Vec<String>,
    params: &mut Vec<DuckValue>,
) {
    if items.is_empty() {
        return;
    }
    let placeholders = std::iter::repeat_n("?", items.len())
        .collect::<Vec<_>>()
        .join(", ");
    clauses.push(format!("{column} IN ({placeholders})"));
    for item in items {
        params.push(DuckValue::Text(item.trim().to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use biocompass_core_traits::OrgId;
    use std::collections::BTreeSet;

    #[test]
    fn enforces_grant_scoping_physical_byte_masking_sql_injection_and_tamper_detection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = seed_demo_hla_catalog(dir.path()).expect("seed catalog");
        assert!(manifest.row_count >= 120);

        let engine = DuckDbCatalogEngine::open_verified(dir.path()).expect("open verified");

        // 1. Collect EVERY plaintext peptide sequence from Full Grant
        let full_grant = DatasetGrant::full_access(OrgId::from("org-owner"), manifest.id.clone());
        let res_full = engine
            .query(
                &full_grant,
                &CatalogQuery {
                    limit: Some(500),
                    ..Default::default()
                },
            )
            .expect("full query");
        assert_eq!(res_full.total_in_scope as u64, manifest.row_count);
        let all_plaintext_sequences: BTreeSet<String> =
            res_full.rows.iter().map(|r| r.sequence.clone()).collect();
        assert!(
            all_plaintext_sequences.len() >= 40,
            "expected >=40 distinct real peptide sequences"
        );
        assert!(all_plaintext_sequences.contains("KLFGTSGQKT"));
        assert!(all_plaintext_sequences.contains("YLEPGPVTA"));

        // 2. Partner Restricted Grant: Lung only + '***' masking
        let partner_grant =
            DatasetGrant::restricted_partner_demo(OrgId::from("org-partner"), manifest.id.clone());
        let res_masked = engine
            .query(
                &partner_grant,
                &CatalogQuery {
                    limit: Some(500),
                    ..Default::default()
                },
            )
            .expect("masked query");
        assert!(res_masked.total_in_scope > 0);
        assert!(res_masked.total_in_scope < res_full.total_in_scope);
        assert!(res_masked.rows.iter().all(|r| r.organ == "Lung"));
        assert!(res_masked.rows.iter().all(|r| r.sequence == "***"));

        // 3. Oracle side-channel via sequence_prefix MUST fail loud
        let probe = engine.query(
            &partner_grant,
            &CatalogQuery {
                sequence_prefix: Some("KLF".into()),
                ..Default::default()
            },
        );
        assert!(matches!(probe, Err(CompassError::AccessDenied(_))));

        // 4. SQL Injection payload in user filter vector is safely bound as a literal -> 0 rows
        let sqli = engine
            .query(
                &partner_grant,
                &CatalogQuery {
                    hla_alleles: vec![
                        "HLA-A*02:01') OR 1=1 UNION SELECT * FROM scoped_cohort --".into(),
                    ],
                    ..Default::default()
                },
            )
            .expect("SQLi payload bound safely as literal parameter");
        assert_eq!(sqli.total_in_scope, 0);
        assert!(sqli.rows.is_empty());

        // 5. Physical Byte-Scan Audit of materialized /in/data/cohort.parquet (ZSTD AND UNCOMPRESSED!)
        let slice_dir = tempfile::tempdir().expect("slice dir");
        let zstd_slice_path = slice_dir.path().join("cohort.parquet");
        let slice = engine
            .materialize_scoped_parquet(&partner_grant, &CatalogQuery::default(), &zstd_slice_path)
            .expect("materialize slice");
        assert_eq!(slice.row_count as usize, res_masked.total_in_scope);
        assert!(slice.sequence_masked);

        let zstd_bytes = std::fs::read(&zstd_slice_path).expect("read zstd parquet bytes");
        let uncompressed_path = slice_dir.path().join("cohort_uncompressed.parquet");
        let inspect_conn = Connection::open_in_memory().expect("inspect duckdb");
        inspect_conn
            .execute_batch(&format!(
                "COPY (SELECT * FROM read_parquet('{}')) TO '{}' (FORMAT PARQUET, COMPRESSION UNCOMPRESSED);",
                zstd_slice_path.display(),
                uncompressed_path.display()
            ))
            .expect("export uncompressed parquet for byte scan");
        let uncompressed_bytes =
            std::fs::read(&uncompressed_path).expect("read uncompressed parquet bytes");

        for secret_seq in &all_plaintext_sequences {
            let needle = secret_seq.as_bytes();
            assert!(
                !zstd_bytes.windows(needle.len()).any(|w| w == needle),
                "LEAK DETECTED: plaintext sequence '{secret_seq}' found in ZSTD cohort.parquet bytes!"
            );
            assert!(
                !uncompressed_bytes.windows(needle.len()).any(|w| w == needle),
                "LEAK DETECTED: plaintext sequence '{secret_seq}' found in UNCOMPRESSED cohort.parquet bytes!"
            );
        }

        // Confirm non-Lung organs ('Skin', 'Liver', 'Colon', 'Kidney') are absent from uncompressed bytes too
        for forbidden_organ in ["Primary_Melanoma", "PMEL", "MAGEA3", "CTNNB1"] {
            let needle = forbidden_organ.as_bytes();
            assert!(
                !uncompressed_bytes.windows(needle.len()).any(|w| w == needle),
                "ROW LEAK DETECTED: out-of-scope token '{forbidden_organ}' found in materialized parquet!"
            );
        }

        // 6. 1-Bit Tamper Detection on underlying Parquet shard
        let shard_file = dir.path().join(&manifest.parquet_files[0].relative_path);
        let mut raw_bytes = std::fs::read(&shard_file).expect("read shard");
        let mid = raw_bytes.len() / 2;
        raw_bytes[mid] ^= 0x01;
        std::fs::write(&shard_file, raw_bytes).expect("write 1-bit tampered shard");

        assert!(matches!(
            DuckDbCatalogEngine::open_verified(dir.path()),
            Err(CompassError::IntegrityViolation(_))
        ));
        assert!(matches!(
            engine.query(&full_grant, &CatalogQuery::default()),
            Err(CompassError::IntegrityViolation(_))
        ));
    }
}
