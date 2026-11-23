//! Trusted query provider for reviewed HLA modules.
//! SQL and table names are fixed by the provider. Request selectors are bound values.

mod sources;
mod sessions;
mod toxicity;
pub use sources::reviewed_hla_input_tables;
#[cfg(test)]
mod session_tests;

use biocompass_core_traits::{CompassError, CompassResult, FilterExpr, ScalarValue};
use biocompass_data_engine::{
    DuckLakeCatalogEngine, SealedSnapshot, build_sealed_table_scan_sql,
};
use duckdb::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use sessions::warm;

// The public helper still represents a single row selector. The provider also
// accepts intersections produced by a worker's local policy.
pub fn sample_scope(scope: Option<&FilterExpr>) -> CompassResult<Option<(&str, Vec<String>)>> {
    scope.map(sample_term).transpose()
}

fn sample_term(expr: &FilterExpr) -> CompassResult<(&str, Vec<String>)> {
    let unsupported = || CompassError::AccessDenied("unsupported private sample scope".into());
    let FilterExpr::In { column, values } = expr else {
        return Err(unsupported());
    };
    let by_class = match column.as_str() {
        "original_sample_number" => false,
        "sample_class" => true,
        _ => return Err(unsupported()),
    };
    if by_class && !(1..=10000).contains(&values.len()) {
        return Err(CompassError::AccessDenied(
            "invalid sample class scope".into(),
        ));
    }
    let allowed = values
        .iter()
        .map(|value| match value {
            ScalarValue::Text(text)
                if !by_class || (!text.trim().is_empty() && text.len() <= 256) =>
            {
                Ok(text.clone())
            }
            _ => Err(unsupported()),
        })
        .collect::<CompassResult<_>>()?;
    Ok((column.as_str(), allowed))
}

#[derive(Default)]
pub(super) struct SampleScope {
    ids: Option<BTreeSet<String>>,
    classes: Option<BTreeSet<String>>,
}

impl SampleScope {
    pub(super) fn parse(scope: Option<&FilterExpr>) -> CompassResult<Self> {
        let mut result = Self::default();
        let mut pending: Vec<_> = scope.into_iter().collect();
        while let Some(expr) = pending.pop() {
            if let FilterExpr::And { exprs } = expr {
                if exprs.is_empty() {
                    return Err(CompassError::AccessDenied(
                        "empty private sample intersection".into(),
                    ));
                }
                pending.extend(exprs);
                continue;
            }
            let (column, values) = sample_term(expr)?;
            let allowed: BTreeSet<_> = values.into_iter().collect();
            let target = if column == "sample_class" {
                &mut result.classes
            } else {
                &mut result.ids
            };
            match target {
                Some(previous) => previous.retain(|value| allowed.contains(value)),
                None => *target = Some(allowed),
            }
        }
        Ok(result)
    }
}

fn bind_scope(
    conn: &Connection,
    engine: &DuckLakeCatalogEngine,
    snapshot: &SealedSnapshot,
    scope: Option<&FilterExpr>,
) -> CompassResult<()> {
    let SampleScope { ids, classes } = SampleScope::parse(scope)?;
    conn.execute_batch(
        "CREATE TEMP TABLE allowed_samples(id VARCHAR PRIMARY KEY);
         CREATE TEMP TABLE request_scope(restricted BOOLEAN);",
    )
    .map_err(query_error)?;
    conn.execute(
        "INSERT INTO request_scope VALUES (?)",
        params![scope.is_some()],
    )
    .map_err(query_error)?;
    let bound_list = |set: BTreeSet<String>| {
        duckdb::types::Value::List(set.into_iter().map(duckdb::types::Value::Text).collect())
    };
    if let Some(classes) = classes {
        // An absent class column or an empty intersection admits no samples.
        if classes.is_empty()
            || ids.as_ref().is_some_and(|set| set.is_empty())
            || !has_columns(
                snapshot,
                "obs_samples",
                &["original_sample_number", "sample_class"],
            )
        {
            return Ok(());
        }
        let table = snapshot
            .tables
            .iter()
            .find(|table| table.schema.name == "obs_samples")
            .expect("columns were checked above");
        let scan = build_sealed_table_scan_sql(table, engine.shard_cache())?;
        let id_filter = if ids.is_some() {
            "AND CAST(original_sample_number AS VARCHAR) IN (SELECT unnest(CAST(? AS VARCHAR[])))"
        } else {
            ""
        };
        let values = std::iter::once(bound_list(classes)).chain(ids.map(bound_list));
        conn.execute(
            &format!(
                "INSERT OR IGNORE INTO allowed_samples
            SELECT CAST(original_sample_number AS VARCHAR) FROM ({scan}) pinned_roster
            WHERE original_sample_number IS NOT NULL
              AND sample_class IN (SELECT unnest(CAST(? AS VARCHAR[]))) {id_filter}"
            ),
            duckdb::params_from_iter(values),
        )
        .map_err(query_error)?;
    } else if let Some(ids) = ids.filter(|set| !set.is_empty()) {
        conn.execute(
            "INSERT OR IGNORE INTO allowed_samples SELECT unnest(CAST(? AS VARCHAR[]))",
            params![bound_list(ids)],
        )
        .map_err(query_error)?;
    }
    Ok(())
}

const CONTRACT: &str = "biocompass.target-validation.v1";
pub const TOXICITY_SCREENING_CONTRACT: &str = "biocompass.toxicity-screening.v1";
pub const OFF_TARGET_TOXICITY_CONTRACT: &str = "biocompass.off-target-toxicity.v1";

pub fn validate_scientific_schema(
    contract: &str,
    snapshot: &SealedSnapshot,
    parameters: &Value,
) -> CompassResult<()> {
    let observations = "obs_sample_peptide_mappings";
    let typing = "obs_sample_hla_typing";
    let typed_sample_columns = &["original_sample_number", "allele_name"][..];
    let mut required = vec![("obs_samples", &["original_sample_number"][..])];
    match contract {
        CONTRACT => {
            let selectors = Selectors::parse(parameters)?;
            required.push((
                observations,
                &["original_sample_number", "sequence", "normalized_intensity"][..],
            ));
            if selectors.kind == "protein" {
                required.push(("reference_proteins", &["accession"][..]));
                required.push((
                    "vocab_protein_peptide_mappings",
                    &["sequence", "accession", "start_position", "end_position"][..],
                ));
            }
            if selectors.hla.is_some() {
                required.push((typing, typed_sample_columns));
            }
        }
        TOXICITY_SCREENING_CONTRACT => {
            required.push(("obs_samples", &["sample_class", "annotation"][..]));
            required.push((
                observations,
                &["original_sample_number", "sequence", "normalized_intensity"][..],
            ));
            if parameters["hla_filter"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty())
            {
                required.push((typing, typed_sample_columns));
            }
        }
        OFF_TARGET_TOXICITY_CONTRACT => {
            required.push(("obs_samples", &["sample_class"][..]));
            required.push((observations, &["original_sample_number", "sequence"][..]));
            required.push(("vocab_peptides", &["sequence"][..]));
            if parameters["hla_filters"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
            {
                required.push((
                    "vocab_peptide_binding_predictions",
                    &["sequence", "allele_name"][..],
                ));
            }
        }
        _ => {
            return Err(CompassError::Validation(
                "Unknown reviewed HLA input contract.".into(),
            ));
        }
    }
    let mut missing = Vec::new();
    for (table, columns) in required {
        if !has_columns(snapshot, table, &[]) {
            missing.push(table.to_owned());
        } else {
            missing.extend(
                columns
                    .iter()
                    .filter(|column| !has_columns(snapshot, table, &[*column]))
                    .map(|column| format!("{table}.{column}")),
            );
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        missing.dedup();
        Err(CompassError::Validation(format!(
            "Missing required inputs for {contract}: {}",
            missing.join(", ")
        )))
    }
}

struct Selectors {
    kind: &'static str,
    term: String,
    hla: Option<String>,
}

impl Selectors {
    fn parse(parameters: &Value) -> CompassResult<Self> {
        let kind = match parameters["search_type"]
            .as_str()
            .unwrap_or("protein")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "peptide" => "peptide",
            "protein" | "gene" | "protein/gene" | "protein-gene" | "" => "protein",
            _ => {
                return Err(CompassError::Validation(
                    "Unsupported target search type.".into(),
                ));
            }
        };
        let term = parameters["search_term"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_uppercase();
        if term.is_empty() || term.chars().count() > 512 {
            return Err(CompassError::Validation(
                "Target search term must contain 1 to 512 characters.".into(),
            ));
        }
        let hla = parameters["hla_filter"]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("null"))
            .map(str::to_owned);
        Ok(Self { kind, term, hla })
    }
}

fn query_error(error: duckdb::Error) -> CompassError {
    CompassError::Storage(format!("Reviewed HLA input query: {error}"))
}

fn has_columns(snapshot: &SealedSnapshot, name: &str, fields: &[&str]) -> bool {
    snapshot.tables.iter().any(|table| {
        table.schema.name == name
            && fields.iter().all(|field| {
                table
                    .schema
                    .columns
                    .iter()
                    .any(|column| column.name == *field)
            })
    })
}

fn register_source(
    conn: &Connection,
    engine: &DuckLakeCatalogEngine,
    snapshot: &SealedSnapshot,
    source: &str,
    alias: &str,
) -> CompassResult<()> {
    let table = snapshot
        .tables
        .iter()
        .find(|table| table.schema.name == source)
        .ok_or_else(|| CompassError::Validation(format!("Target Validation requires {source}.")))?;
    let scan = build_sealed_table_scan_sql(table, engine.shard_cache())?;
    let query = if table
        .schema
        .columns
        .iter()
        .any(|column| column.name == "original_sample_number")
    {
        format!("SELECT scoped.* FROM ({scan}) scoped WHERE NOT (SELECT restricted FROM request_scope)
                 OR EXISTS (SELECT 1 FROM allowed_samples WHERE id=CAST(scoped.original_sample_number AS VARCHAR))")
    } else {
        scan
    };
    conn.execute_batch(&format!("CREATE TEMP VIEW {alias} AS {query};"))
        .map_err(query_error)
}

// The callers supply static names from the provider, never manifest or request strings.
fn text_column(snapshot: &SealedSnapshot, table: &str, column: &str) -> String {
    if has_columns(snapshot, table, &[column]) {
        format!("CAST({column} AS VARCHAR) AS {column}")
    } else {
        format!("NULL::VARCHAR AS {column}")
    }
}

fn register_roster(
    conn: &Connection,
    engine: &DuckLakeCatalogEngine,
    snapshot: &SealedSnapshot,
    selectors: &Selectors,
) -> CompassResult<()> {
    let samples = "obs_samples";
    register_source(conn, engine, snapshot, samples, "sample_rows")?;
    let metadata = ["sample_class", "sample_name", "annotation"]
        .map(|field| text_column(snapshot, samples, field))
        .join(", ");
    let typing = "obs_sample_hla_typing";
    if sources::TYPING.available(snapshot) {
        register_source(conn, engine, snapshot, typing, "typing_rows")?;
        conn.execute_batch(
            "CREATE TEMP VIEW sample_typing AS
               SELECT original_sample_number AS sample_number,
                      string_agg(DISTINCT hla_name(allele_name), ', ' ORDER BY hla_name(allele_name)) AS sample_hla_typing
               FROM typing_rows GROUP BY original_sample_number;",
        )
        .map_err(query_error)?;
    } else if selectors.hla.is_some() {
        return Err(CompassError::Validation(
            "An HLA filter requires sample HLA typing.".into(),
        ));
    } else {
        conn.execute_batch(
            "CREATE TEMP VIEW sample_typing AS
             SELECT NULL::VARCHAR AS sample_number, NULL::VARCHAR AS sample_hla_typing WHERE false;",
        )
        .map_err(query_error)?;
    }
    conn.execute_batch(&format!(
        "CREATE TEMP VIEW eligible_samples AS
           SELECT original_sample_number AS sample_number, {metadata}, sample_hla_typing
           FROM sample_rows LEFT JOIN sample_typing ON original_sample_number = sample_number
           CROSS JOIN target_args
           WHERE hla_filter IS NULL OR contains(lower(sample_hla_typing), lower(hla_filter));"
    ))
    .map_err(query_error)
}

fn register_proteins(
    conn: &Connection,
    engine: &DuckLakeCatalogEngine,
    snapshot: &SealedSnapshot,
    selectors: &Selectors,
) -> CompassResult<bool> {
    let proteins = "reference_proteins";
    let mappings = "vocab_protein_peptide_mappings";
    let available = sources::PROTEIN_ANNOTATION.available(snapshot);
    if !available {
        if selectors.kind == "protein" {
            return Err(CompassError::Validation(
                "Protein search requires peptide-to-protein mappings.".into(),
            ));
        }
        conn.execute_batch(
            "CREATE TEMP VIEW target_proteins AS
             SELECT NULL::VARCHAR AS protein_id, NULL::VARCHAR AS gene_name,
                    NULL::VARCHAR AS protein_name WHERE false;",
        )
        .map_err(query_error)?;
        return Ok(false);
    }
    register_source(conn, engine, snapshot, proteins, "protein_rows")?;
    register_source(conn, engine, snapshot, mappings, "mapping_rows")?;
    let gene = text_column(snapshot, proteins, "gene_name");
    let name = text_column(snapshot, proteins, "protein_name");
    let predicate = if selectors.kind == "protein" {
        "upper(p.gene_name) = term OR upper(p.protein_id) = term"
    } else {
        "EXISTS (
           SELECT 1 FROM mapping_rows m
           WHERE accession_name(m.accession) = p.protein_id AND m.sequence = term
             AND EXISTS (SELECT 1 FROM observation_rows o WHERE o.sequence = m.sequence)
         )"
    };
    conn.execute_batch(&format!(
        "CREATE TEMP VIEW target_proteins AS
           SELECT DISTINCT p.protein_id, p.gene_name, p.protein_name
           FROM (
             SELECT accession_name(accession) AS protein_id, {gene}, {name}
             FROM protein_rows
           ) p CROSS JOIN target_args WHERE ({predicate})
             AND (NOT (SELECT restricted FROM request_scope) OR EXISTS (
                SELECT 1 FROM mapping_rows m JOIN observation_rows o ON o.sequence=m.sequence
                WHERE accession_name(m.accession)=p.protein_id
             ));"
    ))
    .map_err(query_error)?;
    Ok(true)
}

fn register_binders(
    conn: &Connection,
    engine: &DuckLakeCatalogEngine,
    snapshot: &SealedSnapshot,
) -> CompassResult<()> {
    let scores = "obs_sample_allele_scoring";
    let predictions = "vocab_peptide_binding_predictions";
    if !sources::can_rank_binders(snapshot) {
        return conn
            .execute_batch(
                "CREATE TEMP VIEW target_binders AS
               SELECT NULL::VARCHAR AS sample_number, NULL::VARCHAR AS peptide,
                      NULL::VARCHAR AS best_predicted_binder WHERE false;",
            )
            .map_err(query_error);
    }
    register_source(conn, engine, snapshot, scores, "scoring_rows")?;
    register_source(conn, engine, snapshot, predictions, "prediction_rows")?;
    // This mirrors the HLA Compass sample_peptide_binders view and its per-sample,
    // per-peptide winner. Never infer a scored binder directly from a typing row.
    conn.execute_batch(
        "CREATE TEMP VIEW target_binders AS
           SELECT h.sample_number, h.peptide, hla_name(sc.allele_name) AS best_predicted_binder
           FROM target_hits h
           JOIN scoring_rows sc ON sc.original_sample_number = h.sample_number
           JOIN prediction_rows p ON p.sequence = h.peptide
             AND hla_name(p.allele_name) = hla_name(sc.scored_allele_name)
             AND p.tool = sc.tool AND p.tool_version = sc.tool_version
           WHERE NOT EXISTS (
             SELECT 1 FROM scoring_rows excluded
             WHERE excluded.original_sample_number = sc.original_sample_number
               AND excluded.tool = sc.tool AND excluded.tool_version = sc.tool_version
               AND excluded.scored_allele_name IS NULL
           )
           QUALIFY row_number() OVER (
             PARTITION BY h.sample_number, h.peptide
             ORDER BY p.raw_score DESC NULLS LAST, hla_name(sc.allele_name)
           ) = 1;",
    )
    .map_err(query_error)
}

fn export(conn: &Connection, out_dir: &Path, filename: &str, query: &str) -> CompassResult<()> {
    let path = out_dir.join(filename);
    let path = path
        .to_str()
        .ok_or_else(|| CompassError::Storage("Invalid HLA input staging path.".into()))?
        .replace('\'', "''");
    conn.execute_batch(&format!("COPY ({query}) TO '{path}' (FORMAT PARQUET);"))
        .map_err(query_error)
}

#[allow(clippy::too_many_arguments)]
pub fn materialize_scientific(
    engine: &DuckLakeCatalogEngine,
    catalog_id: &str,
    dataset_version: u64,
    snapshot_id: u64,
    digest: &str,
    contract: &str,
    parameters: &Value,
    out_dir: &Path,
    scope: Option<&FilterExpr>,
) -> CompassResult<()> {
    let snapshot = engine.load_sealed_snapshot(Some(snapshot_id))?;
    if snapshot.catalog_id.as_str() != catalog_id
        || snapshot.dataset_digest != digest
        || snapshot.snapshot_id != snapshot_id
        || engine.summary().version != dataset_version.to_string()
    {
        return Err(CompassError::IntegrityViolation(
            "HLA input dataset differs from the pinned release.".into(),
        ));
    }
    validate_scientific_schema(contract, &snapshot, parameters)?;
    warm(|conn| {
        bind_scope(conn, engine, &snapshot, scope)?;
        register_normalizers(conn)?;
        match contract {
            TOXICITY_SCREENING_CONTRACT => {
                return toxicity::screening(conn, engine, &snapshot, dataset_version, out_dir);
            }
            OFF_TARGET_TOXICITY_CONTRACT => {
                return toxicity::off_target(conn, engine, &snapshot, dataset_version, out_dir);
            }
            _ => {}
        }
        let selectors = Selectors::parse(parameters)?;
        conn.execute_batch("CREATE TEMP TABLE target_args (term VARCHAR, hla_filter VARCHAR);")
            .map_err(query_error)?;
        conn.execute(
            "INSERT INTO target_args VALUES (?, ?)",
            params![selectors.term, selectors.hla],
        )
        .map_err(query_error)?;
        register_roster(&conn, &engine, &snapshot, &selectors)?;
        register_source(
            &conn,
            &engine,
            &snapshot,
            "obs_sample_peptide_mappings",
            "observation_rows",
        )?;
        register_proteins(&conn, &engine, &snapshot, &selectors)?;
        let target = if selectors.kind == "protein" {
            "EXISTS (SELECT 1 FROM mapping_rows m JOIN target_proteins p
                  ON accession_name(m.accession) = p.protein_id WHERE m.sequence = o.sequence)"
        } else {
            "o.sequence = term"
        };
        // Group before consulting protein mappings. A peptide mapping to several
        // isoforms cannot multiply a measured intensity or a sample denominator.
        conn.execute_batch(&format!(
            "CREATE TEMP VIEW target_hits AS
           SELECT o.original_sample_number AS sample_number, o.sequence AS peptide,
                  sum(o.normalized_intensity::DOUBLE) AS normalized_intensity
           FROM observation_rows o CROSS JOIN target_args WHERE {target}
           GROUP BY o.original_sample_number, o.sequence;"
        ))
        .map_err(query_error)?;
        register_binders(&conn, &engine, &snapshot)?;

        export(
            &conn,
            out_dir,
            "samples.parquet",
            "
        SELECT sample_number, sample_class, sample_name, annotation, sample_hla_typing
        FROM eligible_samples ORDER BY sample_number",
        )?;
        export(
            &conn,
            out_dir,
            "observations.parquet",
            "
        SELECT h.sample_number, h.peptide, h.normalized_intensity, b.best_predicted_binder
        FROM target_hits h SEMI JOIN eligible_samples s USING (sample_number)
        LEFT JOIN target_binders b USING (sample_number, peptide)
        ORDER BY h.sample_number, h.peptide",
        )?;
        export(
            &conn,
            out_dir,
            "proteins.parquet",
            "
        SELECT protein_id, gene_name, protein_name FROM target_proteins
        ORDER BY protein_id, gene_name, protein_name",
        )?;
        // The reference positional query depends on the selected protein and the
        // authorized catalog. Its count is independent of the plot's HLA filter.
        let position_sql = if selectors.kind == "protein" {
            "WITH observed AS (
           SELECT peptide, count(*) AS peptide_count FROM target_hits GROUP BY peptide
         ), binder_counts AS (
           SELECT peptide, best_predicted_binder, count(*) AS wins
           FROM target_binders WHERE best_predicted_binder IS NOT NULL
           GROUP BY peptide, best_predicted_binder
         ), binders AS (
           SELECT peptide, string_agg(best_predicted_binder || ' (' || wins || ')',
                  '; ' ORDER BY wins DESC, best_predicted_binder) AS best_predicted_binders
           FROM binder_counts GROUP BY peptide
         )
         SELECT DISTINCT m.sequence AS peptide_seq, p.protein_id,
                m.start_position AS first_aa_position, m.end_position AS last_aa_position,
                o.peptide_count, coalesce(b.best_predicted_binders, '') AS best_predicted_binders
         FROM mapping_rows m JOIN target_proteins p ON accession_name(m.accession) = p.protein_id
         JOIN observed o ON m.sequence = o.peptide
         LEFT JOIN binders b ON m.sequence = b.peptide
         ORDER BY peptide_seq, protein_id, first_aa_position, last_aa_position"
        } else {
            "SELECT NULL::VARCHAR AS peptide_seq, NULL::VARCHAR AS protein_id,
                NULL::BIGINT AS first_aa_position, NULL::BIGINT AS last_aa_position,
                NULL::BIGINT AS peptide_count, NULL::VARCHAR AS best_predicted_binders WHERE false"
        };
        export(&conn, out_dir, "positions.parquet", position_sql)?;

        let marker = json!({
            "contract": CONTRACT,
            "catalog_id": catalog_id,
            "version": dataset_version,
            "snapshot_id": snapshot_id,
            "dataset_digest": digest,
            "search_type": selectors.kind,
            "search_term": selectors.term,
            "hla_filter": selectors.hla,
        });
        std::fs::write(
            out_dir.join("target_validation.json"),
            serde_json::to_vec(&marker)
                .map_err(|error| CompassError::Internal(error.to_string()))?,
        )
        .map_err(|error| CompassError::Storage(format!("Write Target Validation marker: {error}")))
    })
}

fn register_normalizers(conn: &Connection) -> CompassResult<()> {
    conn.execute_batch(
        "CREATE TEMP MACRO hla_name(raw) AS CASE
           WHEN nullif(trim(raw), '') IS NULL THEN NULL
           WHEN starts_with(upper(trim(raw)), 'HLA-') THEN upper(trim(raw))
           ELSE 'HLA-' || upper(trim(raw))
         END;
         CREATE TEMP MACRO accession_name(raw) AS
           coalesce(nullif(split_part(trim(raw), '|', 2), ''), trim(raw));",
    )
    .map_err(query_error)
}

/// Selectors are bound values; an OCI module can never supply SQL or join identifiers.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeReadParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_sample_id: Option<String>,
}

const MAX_PAGE_ROWS: usize = 1000;

#[derive(Serialize)]
pub struct NamedReadPage {
    pub rows: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_after_sample_id: Option<String>,
}

fn is_paged_read(query: &str) -> bool {
    matches!(query, "sample_roster.v2" | "sample_observation_counts.v2")
}

pub fn is_runtime_read(query: &str) -> bool {
    sources::runtime_read_source(query).is_some()
}

pub fn runtime_named_read(
    catalog_dir: &Path,
    snapshot_id: u64,
    digest: &str,
    scope: Option<&FilterExpr>,
    query: &str,
    parameters: &RuntimeReadParams,
) -> CompassResult<Value> {
    let engine = DuckLakeCatalogEngine::open_hospital_readonly(catalog_dir)?;
    runtime_named_read_on_catalog(&engine, snapshot_id, digest, scope, query, parameters)
}

/// Uses the worker's verified, already resolved snapshot and shared shard cache.
pub fn runtime_named_read_on_catalog(
    engine: &DuckLakeCatalogEngine,
    snapshot_id: u64,
    digest: &str,
    scope: Option<&FilterExpr>,
    query: &str,
    parameters: &RuntimeReadParams,
) -> CompassResult<Value> {
    let page = runtime_read_page_on_catalog(engine, snapshot_id, digest, scope, query, parameters)?;
    Ok(if is_paged_read(query) { json!(page) } else { json!(page.rows) })
}

pub(crate) fn runtime_read_page_on_catalog(
    engine: &DuckLakeCatalogEngine,
    snapshot_id: u64,
    digest: &str,
    scope: Option<&FilterExpr>,
    query: &str,
    parameters: &RuntimeReadParams,
) -> CompassResult<NamedReadPage> {
    let valid_value = |value: Option<&str>| {
        value.is_none_or(|value| !value.is_empty() && value.len() <= 256)
    };
    if !is_runtime_read(query)
        || !valid_value(parameters.sample_id.as_deref())
        || !valid_value(parameters.sample_class.as_deref())
        || (query == "sample_observation_counts.v1" && parameters.sample_id.is_none())
    {
        return Err(CompassError::Validation("invalid declared query or selectors".into()));
    }
    read_page(engine, snapshot_id, digest, scope, query, parameters)
}

/// The existing rows API accepts only queries exposed for direct access.
pub fn named_read(
    catalog_dir: &Path,
    snapshot_id: u64,
    digest: &str,
    scope: Option<&FilterExpr>,
    query: &str,
    class: Option<&str>,
) -> CompassResult<Value> {
    let parameters = RuntimeReadParams {
        sample_class: class.map(str::to_owned),
        ..RuntimeReadParams::default()
    };
    if is_paged_read(query) {
        return Err(CompassError::Validation("paged reads require the paged interface".into()));
    }
    Ok(json!(direct_named_read_page(catalog_dir, snapshot_id, digest, scope, query, &parameters)?.rows))
}

pub fn direct_named_read_page(
    catalog_dir: &Path,
    snapshot_id: u64,
    digest: &str,
    scope: Option<&FilterExpr>,
    query: &str,
    parameters: &RuntimeReadParams,
) -> CompassResult<NamedReadPage> {
    if !matches!(query, "sample_roster.v1" | "sample_roster.v2" | "cohort_observation_counts.v1")
        || parameters.sample_id.is_some()
    {
        return Err(CompassError::Validation("unknown direct query or selectors".into()));
    }
    let engine = DuckLakeCatalogEngine::open_hospital_readonly(catalog_dir)?;
    read_page(&engine, snapshot_id, digest, scope, query, parameters)
}

fn read_page(
    engine: &DuckLakeCatalogEngine,
    snapshot_id: u64,
    digest: &str,
    scope: Option<&FilterExpr>,
    query: &str,
    parameters: &RuntimeReadParams,
) -> CompassResult<NamedReadPage> {
    let paged = is_paged_read(query);
    let page_size = parameters.page_size.unwrap_or(MAX_PAGE_ROWS);
    if !(1..=MAX_PAGE_ROWS).contains(&page_size)
        || parameters.after_sample_id.as_deref() == Some("")
        || (!paged && (parameters.page_size.is_some() || parameters.after_sample_id.is_some()))
    {
        return Err(CompassError::Validation("invalid pagination parameters".into()));
    }
    let snapshot = engine.load_sealed_snapshot(Some(snapshot_id))?;
    if snapshot.dataset_digest != digest {
        return Err(CompassError::IntegrityViolation(
            "named read dataset digest mismatch".into(),
        ));
    }
    warm(|conn| {
        bind_scope(conn, engine, &snapshot, scope)?;
        register_source(conn, engine, &snapshot, "obs_samples", "sample_rows")?;
        let sample_class = text_column(&snapshot, "obs_samples", "sample_class");
        conn.execute_batch(&format!(
            "CREATE TEMP VIEW roster AS SELECT CAST(original_sample_number AS VARCHAR)
            AS sample_id, {sample_class} FROM sample_rows"
        ))
        .map_err(query_error)?;
        // Apply the cursor to the authorized roster before joining observations. Counts
        // include zero-hit samples, including at a page boundary.
        let selection = match query {
            "sample_roster.v1" | "sample_roster.v2" => {
                "SELECT sample_id, sample_class FROM selected ORDER BY sample_id"
            }
            "cohort_observation_counts.v1" => {
                register_source(conn, engine, &snapshot, "obs_sample_peptide_mappings", "observation_rows")?;
                "SELECT sample_class, count(DISTINCT sample_id), count(o.original_sample_number)
                 FROM selected LEFT JOIN observation_rows o ON CAST(o.original_sample_number AS VARCHAR)=selected.sample_id
                 GROUP BY sample_class ORDER BY sample_class"
            }
            "sample_observation_counts.v1" | "sample_observation_counts.v2" => {
                register_source(conn, engine, &snapshot, "obs_sample_peptide_mappings", "observation_rows")?;
                "SELECT sample_id, count(o.original_sample_number)
                 FROM selected LEFT JOIN observation_rows o ON CAST(o.original_sample_number AS VARCHAR)=selected.sample_id
                 GROUP BY sample_id ORDER BY sample_id"
            }
            _ => return Err(CompassError::Validation("unknown declared query".into())),
        };
        // Bulk counts aggregate only the next page of eligible sample IDs.
        let page_window = if paged { "ORDER BY sample_id LIMIT $4" } else { "" };
        let sql = format!(
            "WITH selected AS (
                SELECT sample_id, sample_class FROM roster
                WHERE (CAST($1 AS VARCHAR) IS NULL OR sample_class = $1)
                  AND (CAST($2 AS VARCHAR) IS NULL OR sample_id = $2)
                  AND (CAST($3 AS VARCHAR) IS NULL OR sample_id > $3)
                {page_window}
             ) {selection} LIMIT $4"
        );
        let mut stmt = conn.prepare(&sql).map_err(query_error)?;
        let mut rows = stmt.query(params![
            parameters.sample_class.as_deref(),
            parameters.sample_id.as_deref(),
            parameters.after_sample_id.as_deref(),
            (page_size + 1) as i64,
        ]).map_err(query_error)?;
        let mut result: Vec<Value> = Vec::new();
        let mut next_after_sample_id = None;
        while let Some(row) = rows.next().map_err(query_error)? {
            if result.len() == page_size {
                if !paged {
                    return Err(CompassError::Validation("named read exceeds row limit".into()));
                }
                let last_id = result.last().and_then(|last| last["sample_id"].as_str())
                    .ok_or_else(|| CompassError::Internal("paged sample has no ID".into()))?;
                next_after_sample_id = Some(last_id.to_owned());
                break;
            }
            result.push(match query {
                "sample_roster.v1" | "sample_roster.v2" => json!({
                    "sample_id": row.get::<_,String>(0).map_err(query_error)?,
                    "sample_class": row.get::<_,Option<String>>(1).map_err(query_error)?
                }),
                "sample_observation_counts.v1" | "sample_observation_counts.v2" => json!({
                    "sample_id": row.get::<_,String>(0).map_err(query_error)?,
                    "observations": row.get::<_,u64>(1).map_err(query_error)?
                }),
                _ => json!({
                    "sample_class": row.get::<_,Option<String>>(0).map_err(query_error)?,
                    "samples": row.get::<_,u64>(1).map_err(query_error)?,
                    "observations": row.get::<_,u64>(2).map_err(query_error)?
                }),
            });
        }
        Ok(NamedReadPage { rows: result, next_after_sample_id })
    })
}
