#!/usr/bin/env python3
"""Regression tests for SEC-20 (compute_only privacy) and SEC-30 ([R7] CSV formula guard)
across bundled BioCompass plugins (`hla-motif-enrichment`, `hla-interactive-explorer`).
"""

from __future__ import annotations

import csv
import importlib.util
import json
import sys
import tempfile
import types
import unittest
from pathlib import Path

PLUGINS_DIR = Path(__file__).resolve().parent


def _load_module(name: str, path: Path, fake_duckdb: types.ModuleType):
    prev = sys.modules.get("duckdb")
    sys.modules["duckdb"] = fake_duckdb
    try:
        spec = importlib.util.spec_from_file_location(name, path)
        assert spec is not None and spec.loader is not None
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        return mod
    finally:
        if prev is None:
            sys.modules.pop("duckdb", None)
        else:
            sys.modules["duckdb"] = prev


class _FakeResult:
    def __init__(self, rows):
        self._rows = rows

    def fetchone(self):
        return self._rows[0] if self._rows else None

    def fetchall(self):
        return list(self._rows)


class _FakeMotifConn:
    def __init__(self, rows):
        self._rows = rows

    def execute(self, sql: str, params=None):
        s = " ".join(sql.split()).upper()
        if "SELECT SEQUENCE FROM READ_PARQUET" in s:
            return _FakeResult([(r["sequence"],) for r in self._rows])
        if "GROUP BY PEPTIDE_LENGTH" in s:
            counts = {}
            for r in self._rows:
                counts[r["peptide_length"]] = counts.get(r["peptide_length"], 0) + 1
            return _FakeResult([(k, v) for k, v in sorted(counts.items()) if v >= 5])
        if "GROUP BY HLA_ALLELE" in s:
            groups = {}
            for r in self._rows:
                groups.setdefault(r["hla_allele"], []).append(r)
            out = []
            for allele, items in groups.items():
                if len(items) >= 5:
                    out.append((allele, len(items), len(items), 12.5, -0.73))
            return _FakeResult(out)
        if "GROUP BY ORGAN, CONDITION" in s:
            groups = {}
            for r in self._rows:
                groups.setdefault((r["organ"], r["condition"]), []).append(r)
            out = []
            for (org, cond), items in groups.items():
                if len(items) >= 5:
                    out.append((org, cond, len(items), 18.2, 14.1))
            return _FakeResult(out)
        # Summary query
        return _FakeResult([(len(self._rows), len(self._rows), 15.0)])


class _FakeExplorerConn:
    def __init__(self, gene_rows):
        self._gene_rows = gene_rows

    def execute(self, sql: str, params=None):
        s = " ".join(sql.split()).upper()
        if "GROUP BY GENE_SYMBOL" in s:
            return _FakeResult(self._gene_rows)
        if "CASE WHEN AFFINITY_NM" in s:
            return _FakeResult([("Ultra-Strong (<=25 nM)", 10)])
        if "SELECT PEPTIDE_LENGTH" in s:
            return _FakeResult([(9, 10)])
        return _FakeResult([(10, len(self._gene_rows), 11.2)])


class BundledPluginSecurityTests(unittest.TestCase):
    def test_sec_20_motif_enrichment_never_leaks_rows_or_sequences_under_compute_only(self):
        secret_seqs = [
            "GILGFVFTL",
            "NLVPMVATV",
            "YLQPRTFLL",
            "FMYSDFHFI",
            "LLWNGPMAV",
            "GLCTLVAML",
        ]
        rows = [
            {
                "peptide_id": f"SECRET_PEP_{idx}",
                "sequence": seq,
                "peptide_length": 9,
                "hla_allele": "HLA-A*02:01",
                "organ": "Lung",
                "condition": "NSCLC",
            }
            for idx, seq in enumerate(secret_seqs)
        ]

        fake_duck = types.ModuleType("duckdb")
        fake_duck.connect = lambda *_a, **_kw: _FakeMotifConn(rows)
        mod = _load_module(
            "hla_motif_analyze_sec20",
            PLUGINS_DIR / "hla-motif-enrichment" / "analyze.py",
            fake_duck,
        )

        # Static manifest check: no /top_candidates view.
        manifest_text = (PLUGINS_DIR / "hla-motif-enrichment" / "plugin.toml").read_text(encoding="utf-8")
        self.assertNotIn("/top_candidates", manifest_text)
        self.assertIn('requires_catalog_level = "compute_only"', manifest_text)

        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            params_path = tmp_path / "params.json"
            cohort_path = tmp_path / "cohort.parquet"
            out_dir = tmp_path / "out"
            out_dir.mkdir()
            params_path.write_text(json.dumps({"alleles": ["HLA-A*02:01"], "min_length": 8, "max_length": 12}))
            cohort_path.write_bytes(b"PAR1")

            rc = mod.run_analysis(params_path, cohort_path, out_dir)
            self.assertEqual(rc, 0)
            result_raw = (out_dir / "result.json").read_text(encoding="utf-8")
            result = json.loads(result_raw)

            self.assertEqual(result["sequence_policy"], "aggregate_compute_only")
            self.assertNotIn("top_candidates", result)
            for idx, seq in enumerate(secret_seqs):
                self.assertNotIn(seq, result_raw)
                self.assertNotIn(f"SECRET_PEP_{idx}", result_raw)
            pwm = result["motif_position_frequencies"]
            self.assertEqual(len(pwm), 9)
            self.assertEqual(pwm[0]["cohort_size"], len(secret_seqs))

        # Small cohort (< 5) suppresses PWM to prevent single-sequence reconstruction.
        small_pwm, small_len = mod.compute_motif_pwm(["GILGFVFTL", "NLVPMVATV"])
        self.assertEqual(small_pwm, [])
        self.assertIsNone(small_len)

    def test_sec_30_r7_csv_formula_guard_in_motif_and_explorer(self):
        # 1. Motif enrichment CSV guard (formulas neutralized, signed numbers preserved).
        formula_allele = '=HYPERLINK("https://evil.test","click")'
        motif_rows = [
            {
                "peptide_id": f"P{i}",
                "sequence": "GILGFVFTL",
                "peptide_length": 9,
                "hla_allele": formula_allele,
                "organ": "Lung",
                "condition": "Healthy",
            }
            for i in range(6)
        ]
        fake_duck_m = types.ModuleType("duckdb")
        fake_duck_m.connect = lambda *_a, **_kw: _FakeMotifConn(motif_rows)
        motif_mod = _load_module(
            "hla_motif_analyze_sec30",
            PLUGINS_DIR / "hla-motif-enrichment" / "analyze.py",
            fake_duck_m,
        )

        # Direct unit checks for [R7] semantics.
        for mod in [motif_mod]:
            self.assertEqual(mod.guard_csv_formula("-0.73"), "-0.73")
            self.assertEqual(mod.guard_csv_formula("+3"), "+3")
            self.assertEqual(mod.guard_csv_formula("-2e-4"), "-2e-4")
            self.assertEqual(mod.guard_csv_formula(-0.73), -0.73)
            self.assertEqual(mod.guard_csv_formula("=1+1"), "'=1+1")
            self.assertEqual(mod.guard_csv_formula("@SUM(A1:A2)"), "'@SUM(A1:A2)")
            self.assertEqual(mod.guard_csv_formula("-cmd|' /C calc'!A0"), "'-cmd|' /C calc'!A0")
            self.assertEqual(mod.guard_csv_formula("+1+1"), "'+1+1")
            self.assertEqual(mod.guard_csv_formula("\t=evil"), "'\t=evil")
            self.assertEqual(mod.guard_csv_formula("\r=evil"), "'\r=evil")

        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            params_path = tmp_path / "params.json"
            cohort_path = tmp_path / "cohort.parquet"
            out_dir = tmp_path / "out"
            out_dir.mkdir()
            params_path.write_text(json.dumps({"alleles": [formula_allele], "min_length": 8, "max_length": 12}))
            cohort_path.write_bytes(b"PAR1")
            self.assertEqual(motif_mod.run_analysis(params_path, cohort_path, out_dir), 0)
            with (out_dir / "artifacts" / "allele_binding_summary.csv").open(encoding="utf-8") as fh:
                csv_rows = list(csv.reader(fh))
            self.assertEqual(csv_rows[1][0], f"'{formula_allele}")
            self.assertEqual(csv_rows[1][4], "-0.73")

        # 2. Interactive explorer CSV guard.
        fake_duck_e = types.ModuleType("duckdb")
        fake_duck_e.connect = lambda *_a, **_kw: _FakeExplorerConn(
            [
                ("=HYPERLINK(\"http://evil\",\"x\")", 6, 4, 12.1, -0.73, 5),
                ("@SUM(1+1)", 5, 3, 18.0, "+3", 4),
                ("-cmd|' /C calc'!A0", 5, 2, 22.5, "-2e-4", 3),
            ]
        )
        explorer_mod = _load_module(
            "hla_explorer_sec30",
            PLUGINS_DIR / "hla-interactive-explorer" / "explore.py",
            fake_duck_e,
        )
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            params_path = tmp_path / "params.json"
            cohort_path = tmp_path / "cohort.parquet"
            out_dir = tmp_path / "out"
            out_dir.mkdir()
            params_path.write_text(json.dumps({"alleles": ["HLA-A*02:01"]}))
            cohort_path.write_bytes(b"PAR1")
            self.assertEqual(explorer_mod.run_explorer(params_path, cohort_path, out_dir), 0)
            with (out_dir / "artifacts" / "gene_hotspots.csv").open(encoding="utf-8") as fh:
                csv_rows = list(csv.reader(fh))
            self.assertTrue(csv_rows[1][0].startswith("'="))
            self.assertEqual(csv_rows[1][4], "-0.73")
            self.assertEqual(csv_rows[2][0], "'@SUM(1+1)")
            self.assertEqual(csv_rows[2][4], "+3")
            self.assertEqual(csv_rows[3][0], "'-cmd|' /C calc'!A0")
            self.assertEqual(csv_rows[3][4], "-2e-4")


if __name__ == "__main__":
    unittest.main()
