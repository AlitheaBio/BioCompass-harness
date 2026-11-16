---
name: hla-interactive-explorer
description: Interactive HLA Antigenic Gene Hotspot & IC50 Affinity Tier Explorer with a custom sandboxed Vite UI communicating over JSON-RPC 2.0 postMessage.
---

# HLA Antigenic Gene Hotspot Explorer (`alithea/hla-interactive-explorer`)

## When to Invoke
Invoke `alithea/hla-interactive-explorer` when ranking antigen source genes (`EGFR`, `KRAS`, `TP53`, `ERBB2`, etc.), inspecting IC50 binding affinity tiers, or exploring cohorts interactively inside the sandboxed Vite UI.

## Security & I/O Contract
- Backend runs inside a rootless/hardened OCI container (`--user 65532:65532 --read-only --network=none`) reading `/in/params.json` + `/in/data/cohort.parquet` and writing `/out/result.json` + `/out/artifacts/gene_hotspots.csv`.
- Custom UI runs inside `<iframe sandbox="allow-scripts">` (`window.origin === "null"`, `CSP: connect-src 'none'`) and communicates exclusively via `@biocompass/plugin-sdk`.
