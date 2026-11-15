#!/usr/bin/env python3
"""Alithea HLA Class I Motif & Cohort Enrichment OCI Plugin.

Contract:
  - Reads /in/params.json and /in/data/cohort.parquet (mounted read-only)
  - Writes /out/result.json and /out/artifacts/allele_binding_summary.csv
"""

from __future__ import annotations

import csv
import json
import sys
from pathlib import Path
import duckdb


def main() -> int:
    params_path = Path("/in/params.json")
    cohort_path = Path("/in/data/cohort.parquet")
    out_dir = Path("/out")
    artifacts_dir = out_dir / "artifacts"

    if not params_path.is_file():
        print("ERROR: missing /in/params.json", file=sys.stderr)
        return 2
    if not cohort_path.is_file():
        print("ERROR: missing /in/data/cohort.parquet", file=sys.stderr)
        return 2

    params = json.loads(params_path.read_text(encoding="utf-8"))
    alleles: list[str] = params.get(
        "alleles",
        ["HLA-A*02:01", "HLA-A*03:01", "HLA-B*07:02", "HLA-B*44:02", "HLA-C*07:02"],
    )
    min_length = int(params.get("min_length", 8))
    max_length = int(params.get("max_length", 12))
    strong_binder_cutoff_nm = float(params.get("strong_binder_cutoff_nm", 50.0))

    print(
        f"[hla-motif-enrichment] Opening pre-scoped cohort {cohort_path} "
        f"(alleles={len(alleles)}, length={min_length}..{max_length} aa, cutoff={strong_binder_cutoff_nm} nM)",
        flush=True,
    )

    con = duckdb.connect(":memory:")
    placeholders = ", ".join(["?"] * len(alleles)) if alleles else "'__NONE__'"

    base_filter = (
        f"peptide_length BETWEEN ? AND ? AND hla_allele IN ({placeholders})"
        if alleles
        else "peptide_length BETWEEN ? AND ?"
    )
    bind_args: list[object] = [min_length, max_length, *alleles]

    summary_row = con.execute(
        f"""
        SELECT
            COUNT(*) AS total_peptides,
            COUNT(*) FILTER (WHERE affinity_nm <= ?) AS strong_binders,
             COALESCE(ROUND(MEDIAN(affinity_nm), 2), 0.0) AS median_affinity_nm,
            BOOL_AND(sequence = '***') AS sequences_masked
        FROM read_parquet('{cohort_path.as_posix()}')
        WHERE {base_filter}
        """,
        [strong_binder_cutoff_nm, *bind_args],
    ).fetchone()

    total_peptides = int(summary_row[0] or 0)
    strong_binders = int(summary_row[1] or 0)
    median_affinity = float(summary_row[2] or 0.0)
    sequences_masked = bool(summary_row[3]) if total_peptides > 0 else False

    length_rows = con.execute(
        f"""
        SELECT peptide_length, COUNT(*) AS n
        FROM read_parquet('{cohort_path.as_posix()}')
        WHERE {base_filter}
        GROUP BY peptide_length
        ORDER BY peptide_length ASC
        """,
        bind_args,
    ).fetchall()

    allele_rows = con.execute(
        f"""
        SELECT
            hla_allele,
            COUNT(*) AS total_n,
            COUNT(*) FILTER (WHERE affinity_nm <= ?) AS strong_n,
            ROUND(MEDIAN(affinity_nm), 2) AS median_nm,
            ROUND(AVG(ms_intensity_log2), 2) AS mean_intensity_log2
        FROM read_parquet('{cohort_path.as_posix()}')
        WHERE {base_filter}
        GROUP BY hla_allele
        ORDER BY strong_n DESC, total_n DESC, hla_allele ASC
        """,
        [strong_binder_cutoff_nm, *bind_args],
    ).fetchall()

    organ_rows = con.execute(
        f"""
        SELECT
            organ,
            condition,
            COUNT(*) AS peptides,
            ROUND(AVG(ms_intensity_log2), 2) AS mean_intensity_log2,
            ROUND(MIN(affinity_nm), 2) AS best_affinity_nm
        FROM read_parquet('{cohort_path.as_posix()}')
        WHERE {base_filter}
        GROUP BY organ, condition
        ORDER BY peptides DESC, organ ASC
        """,
        bind_args,
    ).fetchall()

    top_rows = con.execute(
        f"""
        SELECT
            peptide_id,
            sequence,
            peptide_length,
            hla_allele,
            gene_symbol,
            organ,
            condition,
            affinity_nm,
            eluted_rank_pct,
            ms_intensity_log2,
            donor_count
        FROM read_parquet('{cohort_path.as_posix()}')
        WHERE {base_filter}
        ORDER BY affinity_nm ASC, peptide_id ASC
        LIMIT 15
        """,
        bind_args,
    ).fetchall()

    artifacts_dir.mkdir(parents=True, exist_ok=True)
    csv_path = artifacts_dir / "allele_binding_summary.csv"
    with csv_path.open("w", newline="", encoding="utf-8") as fh:
        writer = csv.writer(fh)
        writer.writerow(
            ["hla_allele", "total_peptides", "strong_binders", "median_affinity_nm", "mean_intensity_log2"]
        )
        for row in allele_rows:
            writer.writerow(row)

    result = {
        "plugin": "alithea/hla-motif-enrichment",
        "version": "1.0.0",
        "sequence_policy": "masked_grant" if sequences_masked else "unmasked_research",
        "summary_cards": [
            {"label": "Matched Ligands", "value": total_peptides},
            {"label": "Strong Binders (<=50 nM)", "value": strong_binders},
            {"label": "Median IC50 (nM)", "value": median_affinity},
            {
                "label": "Sequence Grant Mode",
                "value": "Masked ('***')" if sequences_masked else "Full IUPAC",
            },
        ],
        "length_distribution": [
            {"label": f"{int(r[0])} aa", "length": int(r[0]), "value": int(r[1])}
            for r in length_rows
        ],
        "allele_counts": [
            {
                "label": str(r[0]),
                "total": int(r[1]),
                "value": int(r[2]),
                "median_nm": float(r[3]),
                "mean_intensity_log2": float(r[4]),
            }
            for r in allele_rows
        ],
        "organ_condition_breakdown": [
            {
                "organ": str(r[0]),
                "condition": str(r[1]),
                "peptides": int(r[2]),
                "mean_intensity_log2": float(r[3]),
                "best_affinity_nm": float(r[4]),
            }
            for r in organ_rows
        ],
        "top_candidates": [
            {
                "peptide_id": str(r[0]),
                "sequence": str(r[1]),
                "peptide_length": int(r[2]),
                "hla_allele": str(r[3]),
                "gene_symbol": str(r[4]),
                "organ": str(r[5]),
                "condition": str(r[6]),
                "affinity_nm": float(r[7]),
                "eluted_rank_pct": float(r[8]),
                "ms_intensity_log2": float(r[9]),
                "donor_count": int(r[10]),
            }
            for r in top_rows
        ],
    }

    (out_dir / "result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(
        f"[hla-motif-enrichment] Completed: {total_peptides} matched ligands, "
        f"{strong_binders} strong binders, masked={sequences_masked}",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
