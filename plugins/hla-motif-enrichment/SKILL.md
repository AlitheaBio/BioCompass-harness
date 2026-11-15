---
name: hla-motif-enrichment
description: Quantify HLA Class I allele presentation, strong-binder IC50 distributions, and 8-14mer length profiles over scoped immunopeptidomics Parquet cohorts.
---

# HLA Class I Motif & Cohort Enrichment (`alithea/hla-motif-enrichment`)

## Scientific Purpose
Evaluates eluted LC-MS/MS immunopeptidomics cohorts staged into `/in/data/cohort.parquet` across selected HLA alleles (`HLA-A*02:01`, `HLA-A*03:01`, `HLA-B*07:02`, `HLA-B*44:02`, `HLA-C*07:02`), computing:
1. Strong-binder counts (`affinity_nm <= strong_binder_cutoff_nm`, default `50.0` nM) and median IC50 per allele.
2. Peptide length distribution across `min_length..=max_length` amino acids.
3. Organ × disease-condition presentation enrichment (`mean_intensity_log2`, `best_affinity_nm`).
4. Top ranked candidate targets while strictly preserving host `DatasetGrant` `'***'` sequence masking.

## Parameters
- `alleles`: Array of HLA allele strings (e.g. `["HLA-A*02:01", "HLA-B*07:02"]`).
- `min_length`: Minimum peptide length in amino acids (`8..=15`, default `8`).
- `max_length`: Maximum peptide length in amino acids (`8..=25`, default `12`).
- `strong_binder_cutoff_nm`: IC50 threshold in nM for strong binders (default `50.0`).
