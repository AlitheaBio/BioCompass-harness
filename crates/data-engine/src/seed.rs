//! Deterministic generator for reference DuckLake HLA-I immunopeptidomics catalogs.

use crate::{CatalogSummary, DuckLakeCatalogEngine};
use biocompass_core_traits::{CatalogId, CompassError, CompassResult};
use std::collections::BTreeMap;
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

/// Populate `<alias>.peptides` with the deterministic 135-row HLA-I multi-tissue reference dataset.
pub(crate) fn populate_demo_peptides_table(
    conn: &duckdb::Connection,
    ducklake_alias: &str,
) -> CompassResult<()> {
    conn.execute_batch(
        "CREATE TABLE temp_seed_peptides (
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
    .map_err(|e| CompassError::Storage(format!("create temp_seed_peptides: {e}")))?;

    let mut row_idx = 0u64;
    {
        let mut appender = conn
            .appender("temp_seed_peptides")
            .map_err(|e| CompassError::Storage(format!("duckdb appender: {e}")))?;

        for cohort_rep in 0..3u32 {
            for group in SEED_ANTIGENS {
                for (p_idx, pep) in group.base_peptides.iter().enumerate() {
                    row_idx += 1;
                    let clean_seq = pep.to_ascii_uppercase();
                    let pep_len = clean_seq.len() as i32;
                    let pep_id = format!("PEP-{row_idx:04}");
                    let jitter = (cohort_rep as f64 * 3.7) + (p_idx as f64 * 4.9);
                    let affinity = ((group.base_affinity_nm + jitter) * 10.0).round() / 10.0;
                    let rank_pct =
                        ((affinity / 220.0).clamp(0.05, 1.95) * 100.0).round() / 100.0;
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

    conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS {ducklake_alias}.peptides;
         CREATE TABLE {ducklake_alias}.peptides AS SELECT * FROM temp_seed_peptides ORDER BY peptide_id;
         DROP TABLE temp_seed_peptides;"
    ))
    .map_err(|e| CompassError::Storage(format!("populate {ducklake_alias}.peptides: {e}")))?;
    Ok(())
}

/// Populate `<alias>.peptides` with a synthetic 30-row public CEF viral/benchmark HLA-I dataset
/// (`alithea/hla-public-demo-v1`, physically isolated from proprietary `alithea/hla-atlas-v2026.1` cohorts, §6.3).
pub(crate) fn populate_public_demo_peptides_table(
    conn: &duckdb::Connection,
    ducklake_alias: &str,
) -> CompassResult<()> {
    const PUBLIC_CEF_GROUPS: &[SeedAntigen] = &[
        SeedAntigen {
            organ: "Blood_PBMC",
            condition: "Public_CEF_Control",
            hla_allele: "HLA-A*02:01",
            gene_symbol: "IAV_M1",
            base_peptides: &["GILGFVFTL", "FMYSDFHFI", "VLSDFKTWL", "AIMDKNIIL", "ILGFVFTLT"],
            base_affinity_nm: 9.4,
            tumor_enriched: false,
        },
        SeedAntigen {
            organ: "Blood_PBMC",
            condition: "Public_CEF_Control",
            hla_allele: "HLA-A*02:01",
            gene_symbol: "CMV_UL83",
            base_peptides: &["NLVPMVATV", "VLAELVKQI", "MLNIPSINV", "RIFAELEGV", "YSEHPTFTSQ"],
            base_affinity_nm: 14.2,
            tumor_enriched: false,
        },
        SeedAntigen {
            organ: "Lymph_Node",
            condition: "Public_CEF_Control",
            hla_allele: "HLA-B*07:02",
            gene_symbol: "CMV_UL83",
            base_peptides: &["TPRVTGGGAM", "RPHERNGFTV", "IPSIQSRAL", "APSHVAAAL", "KPAVTTVGL"],
            base_affinity_nm: 19.8,
            tumor_enriched: false,
        },
    ];

    conn.execute_batch(
        "CREATE TABLE temp_public_demo_peptides (
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
    .map_err(|e| CompassError::Storage(format!("create temp_public_demo_peptides: {e}")))?;

    let mut row_idx = 0u64;
    {
        let mut appender = conn
            .appender("temp_public_demo_peptides")
            .map_err(|e| CompassError::Storage(format!("duckdb appender: {e}")))?;

        for rep in 0..2u32 {
            for group in PUBLIC_CEF_GROUPS {
                for (p_idx, pep) in group.base_peptides.iter().enumerate() {
                    row_idx += 1;
                    let clean_seq = pep.to_ascii_uppercase();
                    let pep_len = clean_seq.len() as i32;
                    let pep_id = format!("DEMO-{row_idx:04}");
                    let affinity = ((group.base_affinity_nm + (rep as f64 * 2.5) + (p_idx as f64 * 3.8)) * 10.0).round() / 10.0;
                    let rank_pct = ((affinity / 200.0).clamp(0.05, 1.50) * 100.0).round() / 100.0;
                    let intensity = ((21.2 + (p_idx as f64 * 0.5) - (rep as f64 * 0.2)) * 100.0).round() / 100.0;
                    let donors = 8 + ((p_idx as i32 + rep as i32) % 7);
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
                            intensity,
                            donors,
                        ])
                        .map_err(|e| CompassError::Storage(format!("append public demo row: {e}")))?;
                }
            }
        }
        appender
            .flush()
            .map_err(|e| CompassError::Storage(format!("flush public demo rows: {e}")))?;
    }

    conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS {ducklake_alias}.peptides;
         CREATE TABLE {ducklake_alias}.peptides AS SELECT * FROM temp_public_demo_peptides ORDER BY peptide_id;
         DROP TABLE temp_public_demo_peptides;"
    ))
    .map_err(|e| CompassError::Storage(format!("populate {ducklake_alias}.peptides: {e}")))?;
    Ok(())
}

/// Materialize a local DuckLake HLA-I immunopeptidomics catalog + sealed snapshot under an explicit `catalog_id`.
pub fn seed_local_hla_catalog_with_id(
    catalog_dir: &Path,
    catalog_id: CatalogId,
    name: &str,
    version: &str,
    description: &str,
) -> CompassResult<CatalogSummary> {
    let manifest_path = catalog_dir.join("catalog.json");
    if manifest_path.is_file()
        && let Ok(raw) = std::fs::read_to_string(&manifest_path)
        && let Ok(existing) = serde_json::from_str::<CatalogSummary>(&raw)
        && existing.id == catalog_id
    {
        return Ok(existing);
    }

    if catalog_dir.exists() {
        let _ = std::fs::remove_dir_all(catalog_dir);
    }
    std::fs::create_dir_all(catalog_dir)
        .map_err(|e| CompassError::Storage(format!("create catalog dir: {e}")))?;

    let mut primary_keys = BTreeMap::new();
    primary_keys.insert("peptides".to_string(), vec!["peptide_id".to_string()]);

    let engine = DuckLakeCatalogEngine::create_local_catalog(
        catalog_dir,
        catalog_id,
        name,
        version,
        description,
        "Homo sapiens (GRCh38)",
        primary_keys,
        format!("Initial sealed {version} cohort"),
        |conn| populate_demo_peptides_table(conn, "dl"),
    )?;

    Ok(engine.summary().clone())
}

/// Materialize the reference multi-organ HLA-I immunopeptidomics DuckLake catalog + sealed snapshot
/// in `catalog_dir` if not already present (re-seeding automatically if a pre-v2 legacy `catalog.json` is detected).
pub fn seed_demo_hla_catalog(catalog_dir: &Path) -> CompassResult<CatalogSummary> {
    seed_local_hla_catalog_with_id(
        catalog_dir,
        CatalogId::from("alithea/hla-atlas-v2026.1"),
        "Alithea HLA-I Multi-Tissue Immunopeptidomics Atlas",
        "2026.1.0",
        "LC-MS/MS eluted HLA Class I ligandome across Lung NSCLC, Primary Melanoma, CRC, and healthy human tissues with NetMHCpan-4.1 binding affinities.",
    )
}
