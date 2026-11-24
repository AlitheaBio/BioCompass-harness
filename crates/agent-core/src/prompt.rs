//! System prompt builder embedding active 3-level `DatasetGrant` scope, DuckLake catalog schemas & facets,
//! and OCI plugin `SKILL.md` operational guidance.

use biocompass_core_traits::DatasetGrant;
use biocompass_data_engine::CatalogSummary;
use biocompass_plugin_spec::LoadedPlugin;
use serde_json::Value;
use std::fmt::Write as _;

/// Build the deterministic system prompt provided to the LLM for a session turn.
pub fn build_system_prompt(
    active_grant: Option<&DatasetGrant>,
    catalogs: &[CatalogSummary],
    plugins: &[LoadedPlugin],
    active_module_context: Option<&Value>,
) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str(
        "You are BioCompass, a computational biology and immunopeptidomics workbench agent.\n\
         \n\
         ## Registered BioCompass Tools (Use Directly — Do Not Search or Guess Signatures)\n\
         - `list_catalogs()` — list registered DuckLake catalogs, sealed snapshots, facets, and active grant policy.\n\
         - `query_catalog({ catalog_id, table?, columns?, filter?, order_by?, limit?, offset?, snapshot_id? })` — execute a grant-scoped, SQL-free `QueryAst` over a `rows`-accessible catalog (`filter` uses typed `FilterExpr` nodes like `{\"op\":\"eq\",\"column\":\"organ\",\"value\":\"Lung\"}` or `{\"op\":\"and\",\"exprs\":[...]}`).\n\
         - `update_module_draft({ plugin_id, params, merge?, open_tab? })` — update live UI parameters or Jupyter notebook cells/title/settings in an open module window without launching a container run.\n\
         - `prepare_plugin_run({ plugin_id, plugin_version?, offer_id?, params, attach_active_grant? })` — request a free signed quote for the selected module version and offer. The workbench applies the execution allowance selected by the scientist. Report the tool’s decision. A quote alone does not indicate that the run started.\n\
         - `list_runs({ plugin_id?, session_only?, status?, limit? })` — list recent plugin execution runs with status, parameters summary, and artifact filenames.\n\
         - `get_run_result({ run_id })` — inspect execution status, structured `result.json`, artifact metadata, and Froglet Schnorr verification status.\n\
         - `get_run_artifact({ run_id, artifact_name })` — read text/JSON/CSV/ipynb contents of a completed run artifact.\n\
         - `scaffold_plugin({ template, plugin_id, display_name, include_vite_ui? })` — scaffold a new OCI plugin (`python-uv`, `r-bioc`, `nextflow`, `rust-cli`).\n\
         \n\
         ## Non-Negotiable Operational Rules\n\
         1. **Immediate Direct Tool Use:**\n\
            - All catalog schemas, facets, active DatasetGrant limits, plugins, `SKILL.md` guides, and open module context are already included below.\n\
            - Call the exact BioCompass tool directly without narration filler like \"Let me discover available tools\" or \"Let me check signatures first\".\n\
            - Ignore any external third-party MCP servers or connectors outside the 8 BioCompass tools above, and never run shell, bash, or local file tools for catalog queries or run inspection.\n\
         2. **Three-Level DuckLake Catalog Access (`none` / `compute_only` / `rows`):**\n\
            - Query catalogs exclusively via `query_catalog` using structured `QueryAst` parameters (never raw SQL). Sequences are never masked with `'***'`.\n\
            - When the active grant has `level = \"compute_only\"`, direct row queries via `query_catalog` are forbidden; use published catalog facets and run approved curated modules via `prepare_plugin_run` instead.\n\
         3. **Response & Formatting Hygiene:**\n\
            - Keep step-by-step planning in your native reasoning/thinking channel rather than mixing rough monologue into the answer.\n\
            - Wrap HLA alleles containing `*` in backticks (e.g., `` `HLA-A*02:01` ``) so single asterisks never form accidental italic spans.\n\
            - Present quantitative results cleanly using wide Markdown tables, concise headings, and fenced code blocks.\n\
            - Use fenced ```mermaid diagrams (e.g., `flowchart`, `sequenceDiagram`, `erDiagram`, `timeline`, `pie`, `xychart-beta`) or fenced ```svg schematics whenever explaining pathways, experimental workflows, architectures, or visual relationships; BioCompass renders them inline.\n\n",
    );

    out.push_str("## Active Dataset Access Grant\n");
    if let Some(grant) = active_grant {
        let _ = writeln!(
            out,
            "- Grant ID: `{}` ({})\n- Catalog: `{}`\n- Access Level: `{}`\n- Row Scope: `{:?}`\n- Daily Row Budget: `{:?}`\n- Max Rows Per Query: `{}`\n- Allow Local Export: `{}`\n",
            grant.grant_id,
            grant.label,
            grant.catalog_id,
            grant.level.as_str(),
            grant.row_scope,
            grant.daily_row_budget,
            grant.max_rows_per_query,
            grant.allow_local_export
        );
    } else {
        out.push_str("- No active dataset grant selected.\n\n");
    }

    out.push_str("## Registered DuckLake Data Catalogs\n");
    if catalogs.is_empty() {
        out.push_str("- None registered.\n\n");
    } else {
        for m in catalogs {
            let table_desc: Vec<String> = m
                .tables
                .iter()
                .map(|t| {
                    let cols: Vec<String> = t
                        .columns
                        .iter()
                        .map(|c| format!("{}:{}", c.name, c.data_type))
                        .collect();
                    format!("{} [{}]", t.name, cols.join(", "))
                })
                .collect();
            let _ = writeln!(
                out,
                "- `{}` — {} ({} rows, snapshot #{}, organism: {})\n  Tables: {}\n  Facets: `{}`\n",
                m.id,
                m.name,
                m.row_count,
                m.latest_snapshot_id,
                m.organism,
                table_desc.join("; "),
                serde_json::to_string(&m.facets).unwrap_or_default()
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
                // The laptop agent quotes the local play ledger: credits, never euros.
                m.estimated_cost_act().display_local_credits(),
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

    if let Some(ctx) = active_module_context
        && !ctx.is_null()
    {
        out.push_str("## Active Open Module Context (Live UI Draft, Parameters & Outputs)\n");
        let serialized = serde_json::to_string_pretty(ctx).unwrap_or_default();
        let capped: String = serialized.chars().take(48_000).collect();
        let _ = writeln!(out, "```json\n{capped}\n```\n");
    }

    out
}
