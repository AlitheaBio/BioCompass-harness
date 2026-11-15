//! Deterministic generator for the reference Alithea HLA-I Multi-Tissue Immunopeptidomics Parquet catalog.

use crate::{CatalogColumnSpec, CatalogManifest, ParquetShardEntry};
use biocompass_core_traits::{CatalogId, CompassError, CompassResult};
use duckdb::Connection;
use sha2::{Digest, Sha256};
use std::path::Path;

struct SeedAntigen {
    organ: &'static str,
    condition: &'static str,
    hla_allele: &'static str,
    gene_symbol: &'static str,
    base_peptides: &'static [&'static str],
    base_affinity_nm: f64,
    tumor_enriched: bool,
}

const SEED_ANTIGENS: &[SeedAntigen] = &[
    SeedAntigen {
        organ: "Lung",
        condition: "NSCLC_Tumor",
        hla_allele: "HLA-A*02:01",
        gene_symbol: "EGFR",
        base_peptides: &["KLFGTSGQKT", "YLLPWQGRV", "GLAGVGVKL", "VLGSGAFGT", "SLNITSLGL"],
        base_affinity_nm: 18.5,
        tumor_enriched: true,
    },
    SeedAntigen {
        organ: "Lung",
        condition: "NSCLC_Tumor",
        hla_allele: "HLA-A*02:01",
        gene_symbol: "KRAS",
        base_peptides: &["KLVVVGADV", "VVVGADGVGK", "GADGVGKSAL", "FLCVFAINNT", "YREQIKRVK"],
        base_affinity_nm: 24.2,
        tumor_enriched: true,
    },
    SeedAntigen {
        organ: "Lung",
        condition: "NSCLC_Tumor",
        hla_allele: "HLA-B*07:02",
        gene_symbol: "TP53",
        base_peptides: &["RPILTIITL", "APRVAPAPA", "SPLPSQAML", "RPHHERCSDS", "LPGSTKRAL"],
        base_affinity_nm: 31.0,
        tumor_enriched: true,
    },
    SeedAntigen {
        organ: "Lung",
        condition: "Healthy_Adjacent",
        hla_allele: "HLA-A*02:01",
        gene_symbol: "NKX2-1",
        base_peptides: &["ALAGVAQAL", "SLGHPPPAL", "VLSGGLAGL", "LLGPPPPGL"],
        base_affinity_nm: 62.4,
        tumor_enriched: false,
    },
    SeedAntigen {
        organ: "Lung",
        condition: "Healthy_Adjacent",
        hla_allele: "HLA-C*07:02",
        gene_symbol: "SFTPB",
        base_peptides: &["RYSVILLDT", "KYPVPLIPV", "NYPLWLHQA", "SYLPLLLGL"],
        base_affinity_nm: 89.0,
        tumor_enriched: false,
    },
    SeedAntigen {
        organ: "Skin",
        condition: "Primary_Melanoma",
        hla_allele: "HLA-A*02:01",
        gene_symbol: "PMEL",
        base_peptides: &["YLEPGPVTA", "KTWGQYWQV", "ITDQVPFSV", "VLPDGQVIWV", "ALDGGNKHFL"],
        base_affinity_nm: 12.8,
        tumor_enriched: true,
    },
    SeedAntigen {
        organ: "Skin",
        condition: "Primary_Melanoma",
        hla_allele: "HLA-A*03:01",
        gene_symbol: "MAGEA3",
        base_peptides: &["KVAELVHFL", "SLFRAVITK", "ALSRKVAEL", "IMPkAGLLI", "TLVEVTLGEV"],
        base_affinity_nm: 27.6,
        tumor_enriched: true,
    },
    SeedAntigen {
        organ: "Liver",
        condition: "Normal_Donor",
        hla_allele: "HLA-A*02:01",
        gene_symbol: "ALB",
        base_peptides: &["SLHTLFGDKL", "ALVLIAFAQY", "FLGSFLYEY", "KVPQVSTPTL"],
        base_affinity_nm: 54.0,
        tumor_enriched: false,
    },
    SeedAntigen {
        organ: "Colon",
        condition: "CRC_Tumor",
        hla_allele: "HLA-B*44:02",
        gene_symbol: "CTNNB1",
        base_peptides: &["AEMEGLLGTL", "SEISGLLQAL", "EEAGAALVKL", "QELMDGLPPV"],
        base_affinity_nm: 41.3,
        tumor_enriched: true,
    },
    SeedAntigen {
        organ: "Kidney",
        condition: "Normal_Donor",
        hla_allele: "HLA-B*07:02",
        gene_symbol: "PAX8",
        base_peptides: &["RPLPGLPGL", "SPVPGSAGL", "KPKVATPGL", "TPRGAAVAL"],
        base_affinity_nm: 76.5,
        tumor_enriched: false,
    },
];

/// Materialize the realistic multi-organ HLA-I immunopeptidomics Parquet dataset and `catalog.json`
/// in `catalog_dir` if not already present.
pub fn seed_demo_hla_catalog(catalog_dir: &Path) -> CompassResult<CatalogManifest> {
    let manifest_path = catalog_dir.join("catalog.json");
    if manifest_path.is_file() {
        let raw = std::fs::read_to_string(&manifest_path)
            .map_err(|e| CompassError::Storage(format!("read existing catalog.json: {e}")))?;
        return serde_json::from_str(&raw)
            .map_err(|e| CompassError::Storage(format!("parse existing catalog.json: {e}")));
    }

    std::fs::create_dir_all(catalog_dir)
        .map_err(|e| CompassError::Storage(format!("create catalog dir: {e}")))?;

    let conn = Connection::open_in_memory()
        .map_err(|e| CompassError::Storage(format!("open duckdb for seed: {e}")))?;

    conn.execute_batch(
        "CREATE TABLE raw_peptides (
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
        );",
    )
    .map_err(|e| CompassError::Storage(format!("create seed table: {e}")))?;

    let mut row_idx = 0u64;
    {
        let mut appender = conn
            .appender("raw_peptides")
            .map_err(|e| CompassError::Storage(format!("duckdb appender: {e}")))?;

        // Generate 3 biological replicate cohorts across all antigen groups (~135 rows)
        for cohort_rep in 0..3u32 {
            for group in SEED_ANTIGENS {
                for (p_idx, pep) in group.base_peptides.iter().enumerate() {
                    row_idx += 1;
                    let clean_seq = pep.to_ascii_uppercase();
                    let pep_len = clean_seq.len() as i32;
                    let pep_id = format!("PEP-{row_idx:04}");
                    let jitter = (cohort_rep as f64 * 3.7) + (p_idx as f64 * 4.9);
                    let affinity = ((group.base_affinity_nm + jitter) * 10.0).round() / 10.0;
                    let rank_pct = ((affinity / 220.0).clamp(0.05, 1.95) * 100.0).round() / 100.0;
                    let intensity = if group.tumor_enriched {
                        23.4 + (p_idx as f64 * 0.65) - (cohort_rep as f64 * 0.25)
                    } else {
                        19.8 + (p_idx as f64 * 0.45)
                    };
                    let donors = if group.tumor_enriched {
                        6 + ((p_idx as i32 + cohort_rep as i32) % 9)
                    } else {
                        12 + ((p_idx as i32 + cohort_rep as i32) % 14)
                    };

                    appender
                        .append_row(duckdb::params![
                            pep_id,
                            clean_seq,
                            pep_len,
                            group.hla_allele,
                            group.gene_symbol,
                            group.organ,
                            group.condition,
                            affinity,
                            rank_pct,
                            (intensity * 100.0).round() / 100.0,
                            donors,
                        ])
                        .map_err(|e| CompassError::Storage(format!("append seed row: {e}")))?;
                }
            }
        }
        appender
            .flush()
            .map_err(|e| CompassError::Storage(format!("flush seed rows: {e}")))?;
    }

    let shard_rel = "peptides_part_000.parquet";
    let shard_path = catalog_dir.join(shard_rel);
    let escaped_shard = shard_path.to_string_lossy().replace('\'', "''");
    conn.execute_batch(&format!(
        "COPY (SELECT * FROM raw_peptides ORDER BY peptide_id) TO '{escaped_shard}' (FORMAT PARQUET, COMPRESSION ZSTD);"
    ))
    .map_err(|e| CompassError::Storage(format!("export parquet shard: {e}")))?;

    let parquet_bytes = std::fs::read(&shard_path)
        .map_err(|e| CompassError::Storage(format!("read parquet shard for sha256: {e}")))?;
    let sha256 = hex::encode(Sha256::digest(&parquet_bytes));

    let manifest = CatalogManifest {
        id: CatalogId::from("alithea/hla-atlas-v2026.1"),
        name: "Alithea HLA-I Multi-Tissue Immunopeptidomics Atlas".into(),
        version: "2026.1.0".into(),
        description: "LC-MS/MS eluted HLA Class I ligandome across Lung NSCLC, Primary Melanoma, CRC, and healthy human tissues with NetMHCpan-4.1 binding affinities.".into(),
        organism: "Homo sapiens (GRCh38)".into(),
        row_count: row_idx,
        columns: vec![
            CatalogColumnSpec { name: "peptide_id".into(), sql_type: "VARCHAR".into(), maskable: false },
            CatalogColumnSpec { name: "sequence".into(), sql_type: "VARCHAR".into(), maskable: true },
            CatalogColumnSpec { name: "peptide_length".into(), sql_type: "INTEGER".into(), maskable: false },
            CatalogColumnSpec { name: "hla_allele".into(), sql_type: "VARCHAR".into(), maskable: false },
            CatalogColumnSpec { name: "gene_symbol".into(), sql_type: "VARCHAR".into(), maskable: false },
            CatalogColumnSpec { name: "organ".into(), sql_type: "VARCHAR".into(), maskable: false },
            CatalogColumnSpec { name: "condition".into(), sql_type: "VARCHAR".into(), maskable: false },
            CatalogColumnSpec { name: "affinity_nm".into(), sql_type: "DOUBLE".into(), maskable: false },
            CatalogColumnSpec { name: "eluted_rank_pct".into(), sql_type: "DOUBLE".into(), maskable: false },
            CatalogColumnSpec { name: "ms_intensity_log2".into(), sql_type: "DOUBLE".into(), maskable: false },
            CatalogColumnSpec { name: "donor_count".into(), sql_type: "INTEGER".into(), maskable: false },
        ],
        parquet_files: vec![ParquetShardEntry {
            relative_path: shard_rel.into(),
            sha256,
            size_bytes: parquet_bytes.len() as u64,
            row_count: row_idx,
        }],
    };

    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| CompassError::Storage(format!("serialize catalog.json: {e}")))?;
    std::fs::write(&manifest_path, manifest_json)
        .map_err(|e| CompassError::Storage(format!("write catalog.json: {e}")))?;

    Ok(manifest)
}
