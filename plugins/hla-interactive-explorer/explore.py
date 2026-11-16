#!/usr/bin/env python3
"""Alithea HLA Antigenic Gene Hotspot & Affinity Tier Explorer OCI Plugin.

Contract:
  - Reads /in/params.json and /in/data/cohort.parquet (mounted read-only)
  - Writes /out/result.json and /out/artifacts/gene_hotspots.csv
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
        ["HLA-A*02:01", "HLA-A*03:01", "HLA-B*07:02", "HLA-B*44:02"],
    )
    max_ic50_nm = float(params.get("max_ic50_nm", 250.0))
    min_donors = int(params.get("min_donors", 1))

    print(
        f"[hla-interactive-explorer] Scanning {cohort_path} "
        f"(alleles={len(alleles)}, max_ic50={max_ic50_nm} nM, min_donors={min_donors})",
        flush=True,
    )

    con = duckdb.connect(":memory:")
    placeholders = ", ".join(["?"] * len(alleles)) if alleles else "'__NONE__'"
    where_sql = (
        f"affinity_nm <= ? AND donor_count >= ? AND hla_allele IN ({placeholders})"
        if alleles
        else "affinity_nm <= ? AND donor_count >= ?"
    )
    bind_args: list[object] = [max_ic50_nm, min_donors, *alleles]

    summary = con.execute(
        f"""
        SELECT
            COUNT(*) AS matched_ligands,
            COUNT(DISTINCT gene_symbol) AS distinct_genes,
            COALESCE(ROUND(MIN(affinity_nm), 2), 0.0) AS best_ic50_nm,
            BOOL_AND(sequence = '***') AS sequences_masked
        FROM read_parquet('{cohort_path.as_posix()}')
        WHERE {where_sql}
        """,
        bind_args,
    ).fetchone()

    matched_ligands = int(summary[0] or 0)
    distinct_genes = int(summary[1] or 0)
    best_ic50_nm = float(summary[2] or 0.0)
    sequences_masked = bool(summary[3]) if matched_ligands > 0 else False

    gene_rows = con.execute(
        f"""
        SELECT
            gene_symbol,
            COUNT(*) AS ligands,
            COUNT(*) FILTER (WHERE affinity_nm <= 50.0) AS strong_binders,
            ROUND(MIN(affinity_nm), 2) AS best_affinity_nm,
            ROUND(AVG(ms_intensity_log2), 2) AS mean_intensity_log2,
            MAX(donor_count) AS max_donors
        FROM read_parquet('{cohort_path.as_posix()}')
        WHERE {where_sql}
        GROUP BY gene_symbol
        ORDER BY strong_binders DESC, ligands DESC, gene_symbol ASC
        LIMIT 12
        """,
        bind_args,
    ).fetchall()

    tier_rows = con.execute(
        f"""
        SELECT
            CASE
                WHEN affinity_nm <= 25.0 THEN 'Ultra-Strong (<=25 nM)'
                WHEN affinity_nm <= 50.0 THEN 'Strong (25-50 nM)'
                WHEN affinity_nm <= 150.0 THEN 'Moderate (50-150 nM)'
                ELSE 'Weak (>150 nM)'
            END AS tier,
            COUNT(*) AS n
        FROM read_parquet('{cohort_path.as_posix()}')
        WHERE {where_sql}
        GROUP BY 1
        ORDER BY MIN(affinity_nm) ASC
        """,
        bind_args,
    ).fetchall()

    length_rows = con.execute(
        f"""
        SELECT peptide_length, COUNT(*) AS n
        FROM read_parquet('{cohort_path.as_posix()}')
        WHERE {where_sql}
        GROUP BY 1
        ORDER BY 1 ASC
        """,
        bind_args,
    ).fetchall()

    artifacts_dir.mkdir(parents=True, exist_ok=True)
    csv_path = artifacts_dir / "gene_hotspots.csv"
    with csv_path.open("w", newline="", encoding="utf-8") as fh:
        writer = csv.writer(fh)
        writer.writerow(
            ["gene_symbol", "ligands", "strong_binders", "best_affinity_nm", "mean_intensity_log2", "max_donors"]
        )
        for row in gene_rows:
            writer.writerow(row)

    result = {
        "plugin": "alithea/hla-interactive-explorer",
        "version": "1.0.0",
        "sequence_policy": "masked_grant" if sequences_masked else "unmasked_research",
        "summary_cards": [
            {"label": "Filtered Ligands", "value": matched_ligands},
            {"label": "Source Genes", "value": distinct_genes},
            {"label": "Best IC50 (nM)", "value": best_ic50_nm},
            {"label": "Grant Mode", "value": "Masked ('***')" if sequences_masked else "Full IUPAC"},
        ],
        "length_distribution": [
            {"label": f"{int(r[0])} aa", "length": int(r[0]), "value": int(r[1])}
            for r in length_rows
        ],
        "allele_counts": [
            {"label": str(r[0]), "value": int(r[2]), "total": int(r[1]), "best_nm": float(r[3])}
            for r in gene_rows
        ],
        "gene_hotspots": [
            {
                "gene_symbol": str(r[0]),
                "ligands": int(r[1]),
                "strong_binders": int(r[2]),
                "best_affinity_nm": float(r[3]),
                "mean_intensity_log2": float(r[4]),
                "max_donors": int(r[5]),
            }
            for r in gene_rows
        ],
        "affinity_tiers": [{"label": str(r[0]), "value": int(r[1])} for r in tier_rows],
    }

    (out_dir / "result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(
        f"[hla-interactive-explorer] Completed: {matched_ligands} ligands across {distinct_genes} genes",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
