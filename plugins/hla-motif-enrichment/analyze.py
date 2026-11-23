#!/usr/bin/env python3
"""Alithea HLA Class I Motif & Cohort Enrichment OCI Plugin.

Contract:
  - Reads /in/params.json and /in/data/cohort.parquet (mounted read-only)
  - Writes /out/result.json and /out/artifacts/allele_binding_summary.csv
  - Curated `compute_only` privacy boundary (SEC-20): emits ONLY aggregate cohort
    counts, allele/organ statistics, and positional motif PWM frequencies (never raw
    peptide rows, peptide IDs, or individual IUPAC sequences).
"""

from __future__ import annotations

import csv
import json
import math
import sys
from collections import Counter
from pathlib import Path
import duckdb

MIN_AGGREGATE_COHORT_SIZE = 5
_FORMULA_PREFIXES = ("=", "+", "-", "@", "\t", "\r")


def guard_csv_formula(value: object) -> object:
    """Apply [R7] non-numeric CSV formula guard (SEC-30).

    Prefixes strings starting with spreadsheet formula triggers (=, +, -, @, \\t, \\r)
    with a leading single quote ONLY when the value does not parse as a finite number.
    Signed numbers (-0.73, +3, -2e-4) and numeric types pass through byte-for-byte.
    """
    if not isinstance(value, str) or not value:
        return value
    if not value.startswith(_FORMULA_PREFIXES):
        return value
    try:
        parsed = float(value.strip())
        if math.isfinite(parsed):
            return value
    except ValueError:
        pass
    return f"'{value}"


def compute_motif_pwm(sequences: list[str], min_cohort: int = MIN_AGGREGATE_COHORT_SIZE) -> tuple[list[dict[str, object]], int | None]:
    """Compute aggregate positional amino-acid frequencies for the modal peptide length.

    Returns ([], None) if fewer than `min_cohort` valid IUPAC sequences share the modal
    length, preventing single-row sequence reconstruction under `compute_only` (SEC-20).
    """
    valid = [
        s.strip().upper()
        for s in sequences
        if isinstance(s, str) and len(s.strip()) >= 8 and s.strip() != "***" and s.strip().isalpha()
    ]
    if len(valid) < min_cohort:
        return [], None
    len_counts = Counter(len(s) for s in valid)
    # Prefer 9-mers when >= min_cohort, else modal length with >= min_cohort.
    modal_len: int | None = None
    if len_counts.get(9, 0) >= min_cohort:
        modal_len = 9
    else:
        for length, cnt in len_counts.most_common():
            if cnt >= min_cohort:
                modal_len = length
                break
    if modal_len is None:
        return [], None

    cohort = [s for s in valid if len(s) == modal_len]
    n = len(cohort)
    if n < min_cohort:
        return [], None

    pwm: list[dict[str, object]] = []
    for pos in range(modal_len):
        counts = Counter(seq[pos] for seq in cohort)
        freqs = {aa: round(cnt / n, 4) for aa, cnt in sorted(counts.items())}
        top_aa, top_cnt = counts.most_common(1)[0]
        pwm.append(
            {
                "position": pos + 1,
                "length": modal_len,
                "cohort_size": n,
                "top_residue": top_aa,
                "top_frequency": round(top_cnt / n, 4),
                "frequencies": freqs,
            }
        )
    return pwm, modal_len


def run_analysis(params_path: Path, cohort_path: Path, out_dir: Path) -> int:
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
            COALESCE(ROUND(MEDIAN(affinity_nm), 2), 0.0) AS median_affinity_nm
        FROM read_parquet(?)
        WHERE {base_filter}
        """,
        [strong_binder_cutoff_nm, cohort_path.as_posix(), *bind_args],
    ).fetchone()

    total_peptides = int(summary_row[0] or 0)
    strong_binders = int(summary_row[1] or 0)
    median_affinity = float(summary_row[2] or 0.0)

    length_rows = con.execute(
        f"""
        SELECT peptide_length, COUNT(*) AS n
        FROM read_parquet(?)
        WHERE {base_filter}
        GROUP BY peptide_length
        HAVING COUNT(*) >= ?
        ORDER BY peptide_length ASC
        """,
        [cohort_path.as_posix(), *bind_args, MIN_AGGREGATE_COHORT_SIZE],
    ).fetchall()

    allele_rows = con.execute(
        f"""
        SELECT
            hla_allele,
            COUNT(*) AS total_n,
            COUNT(*) FILTER (WHERE affinity_nm <= ?) AS strong_n,
            ROUND(MEDIAN(affinity_nm), 2) AS median_nm,
            ROUND(AVG(ms_intensity_log2), 2) AS mean_intensity_log2
        FROM read_parquet(?)
        WHERE {base_filter}
        GROUP BY hla_allele
        HAVING COUNT(*) >= ?
        ORDER BY strong_n DESC, total_n DESC, hla_allele ASC
        """,
        [strong_binder_cutoff_nm, cohort_path.as_posix(), *bind_args, MIN_AGGREGATE_COHORT_SIZE],
    ).fetchall()

    organ_rows = con.execute(
        f"""
        SELECT
            organ,
            condition,
            COUNT(*) AS peptides,
            ROUND(AVG(ms_intensity_log2), 2) AS mean_intensity_log2,
            ROUND(MEDIAN(affinity_nm), 2) AS median_affinity_nm
        FROM read_parquet(?)
        WHERE {base_filter}
        GROUP BY organ, condition
        HAVING COUNT(*) >= ?
        ORDER BY peptides DESC, organ ASC
        """,
        [cohort_path.as_posix(), *bind_args, MIN_AGGREGATE_COHORT_SIZE],
    ).fetchall()

    # SEC-20: Compute aggregate motif PWM strictly in-memory without emitting individual
    # sequences or row-level candidate records.
    seq_rows = con.execute(
        f"""
        SELECT sequence
        FROM read_parquet(?)
        WHERE {base_filter}
        """,
        [cohort_path.as_posix(), *bind_args],
    ).fetchall()
    motif_pwm, motif_length = compute_motif_pwm([str(r[0]) for r in seq_rows if r[0] is not None])

    artifacts_dir.mkdir(parents=True, exist_ok=True)
    csv_path = artifacts_dir / "allele_binding_summary.csv"
    with csv_path.open("w", newline="", encoding="utf-8") as fh:
        writer = csv.writer(fh)
        header = [
            "hla_allele",
            "total_peptides",
            "strong_binders",
            "median_affinity_nm",
            "mean_intensity_log2",
        ]
        writer.writerow([guard_csv_formula(cell) for cell in header])
        for row in allele_rows:
            writer.writerow([guard_csv_formula(cell) for cell in row])

    result: dict[str, object] = {
        "plugin": "alithea/hla-motif-enrichment",
        "version": "2.0.0",
        "sequence_policy": "aggregate_compute_only",
        "min_aggregate_cohort_size": MIN_AGGREGATE_COHORT_SIZE,
        "summary_cards": [
            {"label": "Matched Ligands", "value": total_peptides},
            {"label": "Strong Binders (<=50 nM)", "value": strong_binders},
            {"label": "Median IC50 (nM)", "value": median_affinity},
            {"label": "Output Policy", "value": "Aggregate Only (compute_only)"},
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
                "median_affinity_nm": float(r[4]),
            }
            for r in organ_rows
        ],
        "motif_length": motif_length,
        "motif_position_frequencies": motif_pwm,
    }
    if not motif_pwm:
        result["motif_suppressed_reason"] = (
            f"Cohort has fewer than {MIN_AGGREGATE_COHORT_SIZE} peptides at a common length; "
            "motif statistics suppressed to preserve compute_only privacy."
        )

    (out_dir / "result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(
        f"[hla-motif-enrichment] Completed: {total_peptides} matched ligands, "
        f"{strong_binders} strong binders, policy=aggregate_compute_only",
        flush=True,
    )
    return 0


def main() -> int:
    return run_analysis(Path("/in/params.json"), Path("/in/data/cohort.parquet"), Path("/out"))


if __name__ == "__main__":
    raise SystemExit(main())
