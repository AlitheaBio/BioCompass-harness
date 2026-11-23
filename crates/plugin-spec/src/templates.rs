//! Clean starter plugin generators for `python-uv`, `r-bioc`, `nextflow`, and `rust-cli`.

use biocompass_core_traits::{CompassError, CompassResult, PluginId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Strictly validate a 2-segment scaffold `plugin_id` (`<namespace>/<slug>`) where both segments
/// match `^[a-z0-9][a-z0-9_-]{0,62}$` (`SEC-24`). Returns `(namespace, slug)`.
pub fn validate_scaffold_plugin_id(plugin_id: &str) -> CompassResult<(&str, &str)> {
    let trimmed = plugin_id.trim();
    if trimmed != plugin_id {
        return Err(CompassError::Validation(
            "plugin_id must not have leading or trailing whitespace".into(),
        ));
    }
    let _ = PluginId::new(trimmed).map_err(CompassError::Validation)?;
    let mut parts = trimmed.split('/');
    let ns = parts.next().unwrap_or("");
    let slug = parts.next().unwrap_or("");
    if parts.next().is_some() || !is_strict_segment(ns) || !is_strict_segment(slug) {
        return Err(CompassError::Validation(
            "plugin_id must be '<namespace>/<slug>' where each segment matches ^[a-z0-9][a-z0-9_-]{0,62}$".into(),
        ));
    }
    Ok((ns, slug))
}

fn is_strict_segment(seg: &str) -> bool {
    let bytes = seg.as_bytes();
    if bytes.is_empty() || bytes.len() > 63 {
        return false;
    }
    let is_alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    is_alnum(bytes[0])
        && bytes
            .iter()
            .all(|&b| is_alnum(b) || matches!(b, b'-' | b'_'))
}

fn validate_display_name(display_name: &str) -> CompassResult<&str> {
    let trimmed = display_name.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err(CompassError::Validation(
            "display_name must be between 1 and 128 characters".into(),
        ));
    }
    if display_name.chars().any(char::is_control) {
        return Err(CompassError::Validation(
            "display_name must not contain control characters or newlines".into(),
        ));
    }
    Ok(trimmed)
}

fn toml_quote(raw: &str) -> String {
    toml::Value::String(raw.to_owned()).to_string()
}

fn html_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

fn js_string_literal(raw: &str) -> String {
    serde_json::to_string(raw)
        .unwrap_or_else(|_| "\"\"".to_owned())
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

/// Supported first-party container starter templates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TemplateKind {
    PythonUv,
    RBioc,
    Nextflow,
    RustCli,
}

impl TemplateKind {
    pub fn parse(raw: &str) -> CompassResult<Self> {
        match raw.trim() {
            "python-uv" => Ok(Self::PythonUv),
            "r-bioc" => Ok(Self::RBioc),
            "nextflow" => Ok(Self::Nextflow),
            "rust-cli" => Ok(Self::RustCli),
            other => Err(CompassError::Validation(format!(
                "unsupported template '{other}'; expected python-uv, r-bioc, nextflow, or rust-cli"
            ))),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PythonUv => "python-uv",
            Self::RBioc => "r-bioc",
            Self::Nextflow => "nextflow",
            Self::RustCli => "rust-cli",
        }
    }
}

/// Generated file map (`relative_path -> utf8_contents`) for a scaffolded plugin directory.
pub fn render_template_files(
    kind: TemplateKind,
    plugin_id: &str,
    display_name: &str,
    include_vite_ui: bool,
) -> CompassResult<BTreeMap<String, String>> {
    let (_ns, slug) = validate_scaffold_plugin_id(plugin_id)?;
    let clean_name = validate_display_name(display_name)?;
    let plugin_id_toml = toml_quote(plugin_id);
    let display_name_toml = toml_quote(clean_name);
    let image_toml = toml_quote(&format!("local/{slug}:dev"));
    let skill_desc_yaml = serde_json::to_string(&format!(
        "Run {clean_name} ({plugin_id}) on scoped Parquet immunopeptidomics cohorts to quantify HLA allele presentation and peptide length distributions."
    ))
    .map_err(|e| CompassError::Internal(format!("encode SKILL.md description: {e}")))?;
    let display_name_html = html_escape(clean_name);
    let plugin_id_js = js_string_literal(plugin_id);
    let mut files = BTreeMap::new();

    let ui_block = if include_vite_ui {
        "\n[ui]\nentry = \"ui/dist/index.html\"\n"
    } else {
        ""
    };

    let manifest = format!(
        r#"id = {plugin_id_toml}
name = {display_name_toml}
version = "0.1.0"
description = "Cloud-agnostic OCI bio-compute module ({template}) reading /in/params.json + /in/data/*.parquet and emitting /out/result.json."
category = "Immunopeptidomics"
execution = "either"
requires_catalog_level = "compute_only"
image = {image_toml}
network = "none"
price_milli_act = 5000

[resources]
cpus = 2
memory_mb = 4096
gpu = 0
timeout_sec = 600
pids_limit = 256

[inputs.schema]
type = "object"
required = ["alleles", "min_length", "max_length"]

[inputs.schema.properties.alleles]
type = "array"
title = "Target HLA Alleles"
default = ["HLA-A*02:01", "HLA-B*07:02"]
items = {{ type = "string" }}

[inputs.schema.properties.min_length]
type = "integer"
title = "Minimum Peptide Length (aa)"
minimum = 8
maximum = 15
default = 8

[inputs.schema.properties.max_length]
type = "integer"
title = "Maximum Peptide Length (aa)"
minimum = 8
maximum = 25
default = 12

[[views]]
id = "summary-metrics"
kind = "metric_cards"
title = "Cohort Binding Summary"
data_pointer = "/summary_cards"

[[views]]
id = "length-dist"
kind = "length_distribution"
title = "Peptide Length Distribution (8-14 aa)"
data_pointer = "/length_distribution"

[[views]]
id = "allele-enrichment"
kind = "bar"
title = "Strong Binders by HLA Allele"
data_pointer = "/allele_counts"
{ui_block}"#,
        template = kind.as_str()
    );
    files.insert("plugin.toml".into(), manifest);

    let skill_md = format!(
        r#"---
name: {slug}
description: {skill_desc_yaml}
---

# {clean_name} (`{plugin_id}`)

## When to Invoke
Invoke `{plugin_id}` when the scientist asks to evaluate HLA class I presentation, filter 8–14mer ligand lengths, or compare organ-specific binder enrichment on a catalog cohort.

## Container I/O Contract
- **Inputs (`/in`, read-only):**
  - `/in/params.json` validated against `[inputs.schema]` in `plugin.toml`
  - `/in/data/cohort.parquet` materialized by `biocompass-data-engine` with active Grant row scope
- **Outputs (`/out`, read-write):**
  - `/out/result.json` structured summary rendered by BioCompass views and hashed into the Froglet BIP340 Schnorr `Receipt`
  - `/out/artifacts/*` supplementary tables or plots
"#
    );
    files.insert("SKILL.md".into(), skill_md);

    match kind {
        TemplateKind::PythonUv => {
            files.insert(
                "Dockerfile".into(),
                r#"FROM ghcr.io/astral-sh/uv:python3.12-bookworm-slim
ENV HOME=/tmp UV_CACHE_DIR=/tmp/uv-cache PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1
WORKDIR /app
COPY pyproject.toml main.py ./
RUN uv sync --no-dev && chmod -R a+rX /app
USER 65532:65532
ENTRYPOINT ["/app/.venv/bin/python", "/app/main.py"]
"#
                .into(),
            );
            files.insert(
                "pyproject.toml".into(),
                format!(
                    r#"[project]
name = "{slug}"
version = "0.1.0"
requires-python = ">=3.12"
dependencies = ["duckdb>=1.1.0"]
"#
                ),
            );
            files.insert(
                "main.py".into(),
                r#"import json
from pathlib import Path
import duckdb

params = json.loads(Path("/in/params.json").read_text(encoding="utf-8"))
cohort_path = Path("/in/data/cohort.parquet")
con = duckdb.connect(":memory:")

min_len = int(params.get("min_length", 8))
max_len = int(params.get("max_length", 12))

allele_rows = con.execute(
    "SELECT hla_allele, COUNT(*) AS n FROM read_parquet(?) WHERE peptide_length BETWEEN ? AND ? GROUP BY 1 ORDER BY 2 DESC",
    [cohort_path.as_posix(), min_len, max_len],
).fetchall()

len_rows = con.execute(
    "SELECT peptide_length, COUNT(*) AS n FROM read_parquet(?) WHERE peptide_length BETWEEN ? AND ? GROUP BY 1 ORDER BY 1 ASC",
    [cohort_path.as_posix(), min_len, max_len],
).fetchall()

result = {
    "summary_cards": [
        {"label": "Matched Peptides", "value": int(sum(r[1] for r in allele_rows))},
        {"label": "Alleles Observed", "value": len(allele_rows)},
    ],
    "length_distribution": [{"label": f"{int(r[0])} aa", "length": int(r[0]), "value": int(r[1])} for r in len_rows],
    "allele_counts": [{"label": str(r[0]), "value": int(r[1])} for r in allele_rows],
}
Path("/out/result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
print(f"[python-uv] Wrote /out/result.json ({len(allele_rows)} alleles)", flush=True)
"#
                .into(),
            );
        }
        TemplateKind::RBioc => {
            files.insert(
                "Dockerfile".into(),
                r#"FROM bioconductor/bioconductor_docker:RELEASE_3_20
WORKDIR /app
RUN R -e "install.packages(c('jsonlite', 'arrow'), repos='https://cloud.r-project.org')"
COPY run.R /app/run.R
USER 65532:65532
ENTRYPOINT ["Rscript", "/app/run.R"]
"#
                .into(),
            );
            files.insert(
                "run.R".into(),
                r#"library(jsonlite)
library(arrow)

params <- fromJSON("/in/params.json")
df <- read_parquet("/in/data/cohort.parquet")
filtered <- df[df$peptide_length >= params$min_length & df$peptide_length <= params$max_length, ]
counts <- as.data.frame(table(filtered$hla_allele))
colnames(counts) <- c("label", "value")

out <- list(
  summary_cards = list(
    list(label = "Matched Peptides", value = nrow(filtered)),
    list(label = "Distinct Alleles", value = nrow(counts))
  ),
  allele_counts = counts
)
write_json(out, "/out/result.json", auto_unbox = TRUE, pretty = TRUE)
"#
                .into(),
            );
        }
        TemplateKind::Nextflow => {
            files.insert(
                "Dockerfile".into(),
                r#"FROM nfcore/base:2.1
WORKDIR /pipeline
COPY main.nf nextflow.config ./
USER 65532:65532
ENTRYPOINT ["nextflow", "run", "/pipeline/main.nf", "-work-dir", "/tmp/nf-work", "--in_dir", "/in", "--out_dir", "/out"]
"#
                .into(),
            );
            files.insert(
                "nextflow.config".into(),
                "process.executor = 'local'\ndag.overwrite = true\n".into(),
            );
            files.insert(
                "main.nf".into(),
                r#"nextflow.enable.dsl=2
params.in_dir = '/in'
params.out_dir = '/out'

process SUMMARIZE_COHORT {
    publishDir "${params.out_dir}", mode: 'copy'
    output:
    path "result.json"
    script:
    """
    python3 -c '
import json, pathlib
p = json.loads(pathlib.Path("${params.in_dir}/params.json").read_text())
pathlib.Path("result.json").write_text(json.dumps({"pipeline": "nextflow-dsl2", "alleles": p.get("alleles", [])}, indent=2))
    '
    """
}

workflow {
    SUMMARIZE_COHORT()
}
"#
                .into(),
            );
        }
        TemplateKind::RustCli => {
            files.insert(
                "Dockerfile".into(),
                format!(
                    r#"FROM rust:1.85-slim-bookworm AS builder
WORKDIR /src
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /src/target/release/{slug} /usr/local/bin/plugin
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/plugin"]
"#
                ),
            );
            files.insert(
                "Cargo.toml".into(),
                format!(
                    r#"[package]
name = "{slug}"
version = "0.1.0"
edition = "2021"

[dependencies]
serde_json = "1.0"
"#
                ),
            );
            files.insert(
                "src/main.rs".into(),
                r#"use std::fs;

fn main() {
    let raw = fs::read_to_string("/in/params.json").expect("read /in/params.json");
    let params: serde_json::Value = serde_json::from_str(&raw).expect("parse params");
    let out = serde_json::json!({
        "summary_cards": [
            { "label": "Engine", "value": "Rust Distroless OCI" },
            { "label": "Requested Alleles", "value": params["alleles"].as_array().map(|a| a.len()).unwrap_or(0) }
        ],
        "allele_counts": []
    });
    fs::write("/out/result.json", serde_json::to_string_pretty(&out).unwrap()).expect("write /out/result.json");
}
"#
                .into(),
            );
        }
    }

    if include_vite_ui {
        files.insert(
            "ui/dist/index.html".into(),
            format!(
                r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none';" />
  <title>{display_name_html} — Sandboxed Vite Plugin View</title>
  <style>
    body {{ font-family: system-ui, -apple-system, sans-serif; margin: 0; padding: 16px; background: #faf9fe; color: #1d1936; }}
    .card {{ background: #fff; border: 1px solid #e2ddf5; border-radius: 10px; padding: 14px 16px; margin-bottom: 12px; }}
    .badge {{ display: inline-block; padding: 2px 8px; border-radius: 999px; font-size: 11px; font-weight: 600; background: #ede9fe; color: #544ba8; }}
    button {{ background: #544ba8; color: #fff; border: none; border-radius: 7px; padding: 8px 14px; font-weight: 600; cursor: pointer; }}
    pre {{ background: #181430; color: #ede9fe; padding: 10px; border-radius: 8px; font-size: 12px; overflow: auto; }}
  </style>
</head>
<body>
  <div class="card">
    <span class="badge">iframe sandbox="allow-scripts" · @biocompass/plugin-sdk</span>
    <h3 style="margin:8px 0 4px">{display_name_html}</h3>
    <p style="margin:0 0 10px;font-size:13px;color:#575279">Zero host cookies or network access. Communicates strictly via typed postMessage JSON-RPC 2.0.</p>
    <button id="invoke-btn">Request Run Quote via Host Bridge</button>
  </div>
  <pre id="out">Waiting for @biocompass/plugin-sdk host handshake...</pre>
  <script>
    const out = document.getElementById('out');
    const PLUGIN_ID = {plugin_id_js};
    let reqSeq = 1;
    window.addEventListener('message', (ev) => {{
      if (ev.data && ev.data.jsonrpc === '2.0') {{
        out.textContent = JSON.stringify(ev.data, null, 2);
      }}
    }});
    window.parent.postMessage({{ jsonrpc: '2.0', id: reqSeq++, method: 'plugin.ready', params: {{ pluginId: PLUGIN_ID }} }}, '*');
    document.getElementById('invoke-btn').addEventListener('click', () => {{
      window.parent.postMessage({{
        jsonrpc: '2.0',
        id: reqSeq++,
        method: 'froglet.prepareRun',
        params: {{ pluginId: PLUGIN_ID, params: {{ alleles: ['HLA-A*02:01', 'HLA-B*07:02'], min_length: 8, max_length: 11 }} }}
      }}, '*');
    }});
  </script>
</body>
</html>
"#
            ),
        );
    }

    Ok(files)
}

/// Write scaffolded plugin files into a brand-new `target_dir` (`SEC-24`: refuses to overwrite existing dirs).
pub fn write_template_to_dir(
    kind: TemplateKind,
    plugin_id: &str,
    display_name: &str,
    include_vite_ui: bool,
    target_dir: &Path,
) -> CompassResult<Vec<String>> {
    let files = render_template_files(kind, plugin_id, display_name, include_vite_ui)?;
    if target_dir.exists() {
        return Err(CompassError::IntegrityViolation(format!(
            "target plugin directory '{}' already exists; refusing to overwrite",
            target_dir.display()
        )));
    }
    if let Some(parent) = target_dir.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| CompassError::Storage(format!("create {}: {e}", parent.display())))?;
    }
    std::fs::create_dir(target_dir).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            CompassError::IntegrityViolation(format!(
                "target plugin directory '{}' already exists; refusing to overwrite",
                target_dir.display()
            ))
        } else {
            CompassError::Storage(format!("create {}: {e}", target_dir.display()))
        }
    })?;

    let mut written = Vec::with_capacity(files.len());
    for (rel, content) in files {
        let full = target_dir.join(&rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CompassError::Storage(format!("create {}: {e}", parent.display())))?;
        }
        std::fs::write(&full, content)
            .map_err(|e| CompassError::Storage(format!("write {}: {e}", full.display())))?;
        written.push(rel);
    }
    Ok(written)
}
