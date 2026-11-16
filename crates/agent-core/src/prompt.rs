//! System prompt builder embedding active `DatasetGrant` scope, DuckDB catalog schemas,
//! and OCI plugin `SKILL.md` operational guidance.

use biocompass_core_traits::DatasetGrant;
use biocompass_data_engine::DuckDbCatalogEngine;
use biocompass_plugin_spec::LoadedPlugin;
use std::fmt::Write as _;

/// Build the deterministic system prompt provided to the LLM for a session turn.
pub fn build_system_prompt(
    active_grant: Option<&DatasetGrant>,
    catalogs: &[DuckDbCatalogEngine],
    plugins: &[LoadedPlugin],
) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str(
        "You are BioCompass, a computational biology and immunopeptidomics workbench agent.\n\
         \n\
         ## Non-Negotiable Architectural Rules\n\
         1. Two-Step Execution Consent Gate (`Prepare -> Confirm`):\n\
            - You can prepare containerized bio-compute runs via `prepare_plugin_run`, which validates parameters against JSON Schema and signs a `froglet/v1` Quote.\n\
            - You CANNOT confirm or execute runs directly; after calling `prepare_plugin_run`, explain the quoted parameters and ACT cost so the user can click Approve & Run.\n\
         2. DuckDB Grant Enforcement:\n\
            - Query catalogs exclusively via `query_catalog` using structured filter parameters.\n\
            - When `mask_peptide_sequences = true`, amino-acid sequences are masked as `'***'` and `sequence_prefix` filtering is forbidden.\n\n",
    );

    out.push_str("## Active Dataset Access Grant\n");
    if let Some(grant) = active_grant {
        let _ = writeln!(
            out,
            "- Grant ID: `{}` ({})\n- Catalog: `{}`\n- Allowed Organs: `{:?}`\n- Allowed Conditions: `{:?}`\n- Sequence Masking (`'***'`): `{}`\n- Max Rows: `{}`\n",
            grant.grant_id,
            grant.label,
            grant.catalog_id,
            grant.allowed_organs,
            grant.allowed_conditions,
            grant.mask_peptide_sequences,
            grant.max_rows
        );
    } else {
        out.push_str("- No active dataset grant selected.\n\n");
    }

    out.push_str("## Registered Parquet Data Catalogs\n");
    if catalogs.is_empty() {
        out.push_str("- None registered.\n\n");
    } else {
        for cat in catalogs {
            let m = cat.manifest();
            let cols: Vec<String> = m
                .columns
                .iter()
                .map(|c| format!("{}:{}", c.name, c.sql_type))
                .collect();
            let _ = writeln!(
                out,
                "- `{}` — {} ({} rows, organism: {})\n  Columns: {}\n",
                m.id,
                m.name,
                m.row_count,
                m.organism,
                cols.join(", ")
            );
        }
    }

    out.push_str("## Registered OCI Bio-Compute Plugins & SKILL.md Guidance\n");
    if plugins.is_empty() {
        out.push_str("- None registered.\n");
    } else {
        for plugin in plugins {
            let m = &plugin.manifest;
            let _ = writeln!(
                out,
                "### `{}` (v{}, cost: {})\n- Description: {}\n- Input JSON Schema: `{}`",
                m.id,
                m.version,
                m.estimated_cost_act().display_act(),
                m.description,
                m.inputs.schema
            );
            if let Some(skill) = &plugin.skill {
                let _ = writeln!(out, "- SKILL.md:\n{}\n", skill.body_markdown);
            } else {
                out.push('\n');
            }
        }
    }

    out
}
