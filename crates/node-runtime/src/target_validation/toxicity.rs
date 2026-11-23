//! Named projections for the reviewed toxicity modules. All sample relations are
//! narrowed at registration before measurements, annotations or binders join.

use super::{
    OFF_TARGET_TOXICITY_CONTRACT, TOXICITY_SCREENING_CONTRACT, export, has_columns, query_error,
    register_source, sources, text_column,
};
use biocompass_core_traits::{CompassError, CompassResult};
use biocompass_data_engine::{DuckLakeCatalogEngine, SealedSnapshot};
use duckdb::Connection;
use serde_json::{Value, json};
use std::path::Path;

fn register_cohort(
    conn: &Connection,
    engine: &DuckLakeCatalogEngine,
    snapshot: &SealedSnapshot,
) -> CompassResult<()> {
    register_source(conn, engine, snapshot, "obs_samples", "sample_rows")?;
    register_source(
        conn,
        engine,
        snapshot,
        "obs_sample_peptide_mappings",
        "observation_rows",
    )?;
    let annotation = text_column(snapshot, "obs_samples", "annotation");
    conn.execute_batch(&format!(
        "CREATE TEMP VIEW roster AS
         SELECT original_sample_number::VARCHAR AS sample_id, sample_class::VARCHAR AS sample_class,
                {annotation}
         FROM sample_rows;
         CREATE TEMP VIEW observed_peptide_samples AS
         SELECT DISTINCT r.sample_id, o.sequence
         FROM observation_rows o JOIN roster r ON r.sample_id = o.original_sample_number::VARCHAR
         WHERE o.sequence IS NOT NULL;"
    ))
    .map_err(query_error)
}

fn register_annotations(
    conn: &Connection,
    engine: &DuckLakeCatalogEngine,
    snapshot: &SealedSnapshot,
) -> CompassResult<()> {
    let proteins = "reference_proteins";
    let mappings = "vocab_protein_peptide_mappings";
    if !sources::PROTEIN_ANNOTATION.available(snapshot) {
        return conn
            .execute_batch(
                "CREATE TEMP VIEW annotations AS
                 SELECT NULL::VARCHAR AS sequence, NULL::VARCHAR[] AS genes,
                        NULL::VARCHAR[] AS proteins, NULL::VARCHAR[] AS uniprots WHERE false;",
            )
            .map_err(query_error);
    }
    register_source(conn, engine, snapshot, proteins, "protein_rows")?;
    register_source(conn, engine, snapshot, mappings, "mapping_rows")?;
    let gene = text_column(snapshot, proteins, "gene_name");
    let protein = text_column(snapshot, proteins, "protein_name");
    // Group mappings per peptide before joining observations. A peptide mapped
    // to multiple proteins still contributes exactly one row per measurement.
    conn.execute_batch(&format!(
        "CREATE TEMP VIEW annotations AS
         WITH annotated AS (
           SELECT m.sequence, p.gene_name, p.protein_name, p.accession
           FROM mapping_rows m JOIN (
             SELECT accession_name(accession) AS accession, {gene}, {protein} FROM protein_rows
           ) p ON accession_name(m.accession) = p.accession
           SEMI JOIN (SELECT DISTINCT sequence FROM observed_peptide_samples) o USING (sequence)
         )
         SELECT sequence,
                list(DISTINCT gene_name ORDER BY gene_name) FILTER (WHERE gene_name IS NOT NULL AND gene_name <> '') AS genes,
                list(DISTINCT protein_name ORDER BY protein_name) FILTER (WHERE protein_name IS NOT NULL AND protein_name <> '') AS proteins,
                list(DISTINCT accession ORDER BY accession) FILTER (WHERE accession IS NOT NULL AND accession <> '') AS uniprots
         FROM annotated GROUP BY sequence;"
    ))
    .map_err(query_error)
}

fn register_prediction_evidence(
    conn: &Connection,
    engine: &DuckLakeCatalogEngine,
    snapshot: &SealedSnapshot,
    export_scores: bool,
) -> CompassResult<()> {
    let predictions = "vocab_peptide_binding_predictions";
    let scoring = "obs_sample_allele_scoring";
    let vocabulary_available = sources::PREDICTIONS.available(snapshot);
    let observed_binders = sources::can_score_binders(snapshot);
    if observed_binders || (export_scores && vocabulary_available) {
        register_source(conn, engine, snapshot, predictions, "prediction_rows")?;
    }
    if export_scores && vocabulary_available {
        let score = if has_columns(snapshot, predictions, &["raw_score"]) {
            "raw_score::DOUBLE"
        } else {
            "NULL::DOUBLE"
        };
        // Predictions are available only for sequences observed in the caller's scope.
        conn.execute_batch(&format!(
            "CREATE TEMP VIEW prediction_max AS
             SELECT sequence, hla_name(allele_name) AS allele, max({score}) AS raw_score
             FROM prediction_rows
             SEMI JOIN (SELECT DISTINCT sequence FROM observed_peptide_samples) o USING (sequence)
             WHERE hla_name(allele_name) IS NOT NULL GROUP BY sequence, hla_name(allele_name);"
        ))
        .map_err(query_error)?;
    } else if export_scores {
        conn.execute_batch(
            "CREATE TEMP VIEW prediction_max AS
             SELECT NULL::VARCHAR AS sequence, NULL::VARCHAR AS allele,
                    NULL::DOUBLE AS raw_score WHERE false;",
        )
        .map_err(query_error)?;
    }
    if !observed_binders {
        return conn
            .execute_batch(
                "CREATE TEMP VIEW observed_binders AS
                 SELECT NULL::VARCHAR AS sample_id, NULL::VARCHAR AS sequence,
                        NULL::VARCHAR AS allele WHERE false;",
            )
            .map_err(query_error);
    }
    register_source(conn, engine, snapshot, scoring, "scoring_rows")?;
    // HLA Compass's scoring version and unscorable-allele exclusion apply
    // before deduplicating sample x sequence x typed allele evidence.
    conn.execute_batch(
        "CREATE TEMP VIEW observed_binders AS
         SELECT DISTINCT o.sample_id, o.sequence, hla_name(sc.allele_name) AS allele
         FROM observed_peptide_samples o
         JOIN scoring_rows sc ON sc.original_sample_number::VARCHAR = o.sample_id
         JOIN prediction_rows p ON p.sequence = o.sequence
           AND hla_name(p.allele_name) = hla_name(sc.scored_allele_name)
           AND p.tool::VARCHAR = sc.tool::VARCHAR
           AND p.tool_version::VARCHAR = sc.tool_version::VARCHAR
         WHERE hla_name(sc.allele_name) IS NOT NULL
           AND NOT EXISTS (
             SELECT 1 FROM scoring_rows excluded
             WHERE excluded.original_sample_number::VARCHAR = o.sample_id
               AND excluded.tool::VARCHAR = sc.tool::VARCHAR
               AND excluded.tool_version::VARCHAR = sc.tool_version::VARCHAR
               AND hla_name(excluded.scored_allele_name) IS NULL
           );",
    )
    .map_err(query_error)
}

fn write_marker(
    out_dir: &Path,
    filename: &str,
    contract: &str,
    snapshot: &SealedSnapshot,
    version: u64,
    samples_visible: u64,
) -> CompassResult<()> {
    let marker: Value = json!({
        "contract": contract,
        "catalog_id": snapshot.catalog_id,
        "version": version,
        "snapshot_id": snapshot.snapshot_id,
        "dataset_digest": snapshot.dataset_digest,
        "samples_visible": samples_visible,
    });
    std::fs::write(
        out_dir.join(filename),
        serde_json::to_vec(&marker).map_err(|error| CompassError::Internal(error.to_string()))?,
    )
    .map_err(|error| CompassError::Storage(format!("Write HLA input marker: {error}")))
}

pub(super) fn screening(
    conn: &Connection,
    engine: &DuckLakeCatalogEngine,
    snapshot: &SealedSnapshot,
    version: u64,
    out_dir: &Path,
) -> CompassResult<()> {
    register_cohort(conn, engine, snapshot)?;
    register_annotations(conn, engine, snapshot)?;
    register_prediction_evidence(conn, engine, snapshot, false)?;
    let typing = "obs_sample_hla_typing";
    if sources::TYPING.available(snapshot) {
        register_source(conn, engine, snapshot, typing, "typing_rows")?;
        conn.execute_batch(
            "CREATE TEMP VIEW sample_typing AS
             SELECT original_sample_number::VARCHAR AS sample_id,
                    list(DISTINCT hla_name(allele_name) ORDER BY hla_name(allele_name))
                      FILTER (WHERE hla_name(allele_name) IS NOT NULL) AS alleles
             FROM typing_rows GROUP BY original_sample_number::VARCHAR;",
        )
        .map_err(query_error)?;
    } else {
        conn.execute_batch(
            "CREATE TEMP VIEW sample_typing AS
             SELECT NULL::VARCHAR AS sample_id, NULL::VARCHAR[] AS alleles WHERE false;",
        )
        .map_err(query_error)?;
    }
    export(
        conn,
        out_dir,
        "samples.parquet",
        "SELECT sample_id, sample_class, annotation, coalesce(alleles, []::VARCHAR[]) AS alleles
         FROM roster LEFT JOIN sample_typing USING (sample_id) ORDER BY sample_id",
    )?;
    export(
        conn,
        out_dir,
        "observations.parquet",
        "WITH binders_by_sample AS (
           SELECT sample_id, sequence, list(allele ORDER BY allele) AS binders
           FROM observed_binders GROUP BY sample_id, sequence
         )
         SELECT r.sample_id, o.sequence AS peptide, o.normalized_intensity::DOUBLE AS normalized_intensity,
                coalesce(a.genes, []::VARCHAR[]) AS genes,
                coalesce(a.uniprots, []::VARCHAR[]) AS uniprots,
                coalesce(a.proteins, []::VARCHAR[]) AS proteins,
                coalesce(b.binders, []::VARCHAR[]) AS binders
         FROM observation_rows o JOIN roster r ON o.original_sample_number::VARCHAR = r.sample_id
         LEFT JOIN annotations a ON a.sequence = o.sequence
         LEFT JOIN binders_by_sample b ON b.sample_id = r.sample_id AND b.sequence = o.sequence
         WHERE o.sequence IS NOT NULL
         ORDER BY r.sample_id, o.sequence, o.normalized_intensity",
    )?;
    let count = conn
        .query_row("SELECT count(*) FROM roster", [], |row| row.get(0))
        .map_err(query_error)?;
    write_marker(
        out_dir,
        "toxicity_screening.json",
        TOXICITY_SCREENING_CONTRACT,
        snapshot,
        version,
        count,
    )
}

pub(super) fn off_target(
    conn: &Connection,
    engine: &DuckLakeCatalogEngine,
    snapshot: &SealedSnapshot,
    version: u64,
    out_dir: &Path,
) -> CompassResult<()> {
    register_cohort(conn, engine, snapshot)?;
    register_annotations(conn, engine, snapshot)?;
    register_prediction_evidence(conn, engine, snapshot, true)?;
    let vocabulary = "vocab_peptides";
    register_source(conn, engine, snapshot, vocabulary, "peptide_vocab")?;
    // Natural-key imports assign one deterministic ID per vocabulary sequence.
    // Restrict output after identifier assignment so a grant does not renumber IDs.
    let id = if has_columns(snapshot, vocabulary, &["peptide_id"]) {
        "peptide_id::BIGINT"
    } else {
        "row_number() OVER (ORDER BY sequence)"
    };
    conn.execute_batch(&format!(
        "CREATE TEMP VIEW peptide_ids AS SELECT sequence, {id} AS peptide_id FROM peptide_vocab;"
    ))
    .map_err(query_error)?;
    export(
        conn,
        out_dir,
        "peptides.parquet",
        "WITH counts AS (
           SELECT o.sequence, count(*) AS total_samples,
             count(*) FILTER (
               WHERE r.sample_class NOT ILIKE '%tumor%'
                 AND r.sample_class NOT ILIKE '%tumour%'
                 AND r.sample_class NOT ILIKE '%cancer%'
             ) AS healthy_samples
           FROM observed_peptide_samples o JOIN roster r USING (sample_id)
           GROUP BY o.sequence
         )
         SELECT p.peptide_id, c.sequence AS peptide,
                coalesce(array_to_string(a.genes, '; '), '') AS gene_names,
                coalesce(array_to_string(a.proteins, '; '), '') AS protein_names,
                coalesce(array_to_string(a.uniprots, '; '), '') AS uniprot_ids,
                c.total_samples, c.healthy_samples
         FROM counts c JOIN peptide_ids p USING (sequence)
         LEFT JOIN annotations a USING (sequence) ORDER BY p.peptide_id",
    )?;
    export(
        conn,
        out_dir,
        "predictions.parquet",
        "WITH counts AS (
           SELECT sequence, allele, count(*) AS sample_count FROM observed_binders
           GROUP BY sequence, allele
         )
         SELECT p.peptide_id, v.allele, v.raw_score, coalesce(c.sample_count, 0) AS sample_count
         FROM prediction_max v JOIN peptide_ids p USING (sequence)
         LEFT JOIN counts c USING (sequence, allele) ORDER BY p.peptide_id, v.allele",
    )?;
    let count = conn
        .query_row("SELECT count(*) FROM roster", [], |row| row.get(0))
        .map_err(query_error)?;
    write_marker(
        out_dir,
        "off_target_toxicity.json",
        OFF_TARGET_TOXICITY_CONTRACT,
        snapshot,
        version,
        count,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target_validation::materialize_scientific;
    use biocompass_core_traits::{CatalogId, FilterExpr};
    use biocompass_data_engine::open_duckdb_with_extensions;

    #[test]
    fn preserves_measurements_and_isolates_warm_sample_scopes() -> CompassResult<()> {
        let temp = tempfile::tempdir().unwrap();
        let id = CatalogId::new("bc-private-v1/toxicity-grain").unwrap();
        let catalog = DuckLakeCatalogEngine::create_local_catalog(
            &temp.path().join("catalog"),
            id.clone(),
            "Toxicity grain",
            "1",
            "",
            "",
            Default::default(),
            "qualified provider fixture",
            |conn| {
                conn.execute_batch(
                    "CREATE TABLE dl.obs_samples AS SELECT * FROM (VALUES
                      ('S1', 'healthy tissue', 'liver'),
                      ('S2', 'tumor tissue', 'lung')
                     ) rows(original_sample_number, sample_class, annotation);
                     CREATE TABLE dl.obs_sample_peptide_mappings AS SELECT * FROM (VALUES
                      ('S1', 'GILGFVFTL', 0.0), ('S1', 'GILGFVFTL', NULL),
                      ('S2', 'SIINFEKL', 3.0)
                     ) rows(original_sample_number, sequence, normalized_intensity);
                     CREATE TABLE dl.vocab_peptides AS
                       SELECT * FROM (VALUES ('GILGFVFTL'), ('SIINFEKL')) rows(sequence);
                     CREATE TABLE dl.reference_proteins AS SELECT * FROM (VALUES
                       ('Q1', 'GENE1', 'Protein 1'), ('Q2', 'GENE2', 'Protein 2')
                     ) rows(accession, gene_name, protein_name);
                     CREATE TABLE dl.vocab_protein_peptide_mappings AS
                       SELECT * FROM (VALUES ('GILGFVFTL', 'Q1'), ('GILGFVFTL', 'Q2'))
                         rows(sequence, accession);
                     CREATE TABLE dl.obs_sample_allele_scoring AS
                       SELECT 'S1' original_sample_number, 'A*02:01' allele_name,
                              'A*02:01' scored_allele_name, 'tool' tool, '1' tool_version;
                     CREATE TABLE dl.vocab_peptide_binding_predictions AS SELECT * FROM (VALUES
                       ('GILGFVFTL', 'A*02:01', 0.9, 'tool', '1'),
                       ('SIINFEKL', 'A*02:01', 0.8, 'tool', '1')
                     ) rows(sequence, allele_name, raw_score, tool, tool_version);",
                )
                .map_err(query_error)
            },
        )?;
        let pin = catalog.load_sealed_snapshot(None)?;
        let project = |folder, contract, sample| {
            let out = temp.path().join(folder);
            std::fs::create_dir(&out).unwrap();
            let scope = FilterExpr::in_values("original_sample_number", [sample]);
            materialize_scientific(
                &catalog,
                id.as_str(),
                1,
                pin.snapshot_id,
                &pin.dataset_digest,
                contract,
                &json!({}),
                &out,
                Some(&scope),
            )?;
            Ok::<_, CompassError>(out)
        };

        let (screened, off) = std::thread::scope(|threads| {
            let a = threads.spawn(|| project("screen", TOXICITY_SCREENING_CONTRACT, "S1"));
            let b = threads.spawn(|| project("off", OFF_TARGET_TOXICITY_CONTRACT, "S2"));
            (a.join().unwrap(), b.join().unwrap())
        });
        let (screened, off) = (screened?, off?);
        let db = open_duckdb_with_extensions(&[])?;
        let measurements = db
            .query_row(
                "SELECT count(*), count(normalized_intensity), sum(normalized_intensity),
                    min(array_length(genes)), min(array_length(binders)),
                    count(DISTINCT sample_id), min(sample_id)
             FROM read_parquet(?)",
                [screened.join("observations.parquet").to_str().unwrap()],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, f64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, String>(6)?,
                    ))
                },
            )
            .map_err(query_error)?;
        assert_eq!(measurements, (2, 1, 0.0, 2, 1, 1, "S1".into()));

        let peptides = db.query_row(
            "SELECT count(*), min(peptide_id), min(peptide), min(total_samples), min(healthy_samples)
             FROM read_parquet(?)",
            [off.join("peptides.parquet").to_str().unwrap()],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?, r.get::<_, i64>(4)?)),
        ).map_err(query_error)?;
        assert_eq!(peptides, (1, 2, "SIINFEKL".into(), 1, 0));
        let binders = db.query_row(
            "SELECT count(*), min(peptide_id), min(allele), min(sample_count) FROM read_parquet(?)",
            [off.join("predictions.parquet").to_str().unwrap()],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?)),
        ).map_err(query_error)?;
        assert_eq!(binders, (1, 2, "HLA-A*02:01".into(), 0));
        Ok(())
    }
}
