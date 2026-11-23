# AGENTS.md

BioCompass (`AlitheaBio/BioCompass-harness`) is a cloud-agnostic Rust 2024 + Vite 8 / React 19 computational biology agent harness and OCI plugin execution workbench built on `froglet-protocol`.

## Hard Repository Boundaries

- Never modify legacy or service reference repositories (`HLA-Compass-platform`, `froglet-services`); `AlitheaBio/froglet` is consumed by pinned commit SHA and receives only the approved v2 `operator_ledger.v1` settlement addition.
- Keep `CLAUDE.md` symlinked to `AGENTS.md`.

## Core Architectural Invariants

1. **Zero Duct Tape & Cloud-Agnostic Traits (`crates/core-traits`):**
   - Domain crates (`plugin-spec`, `data-engine`, `oci-runner`, `node-runtime`, `agent-core`) never branch on cloud vendor SDKs or `if is_cloud`.
   - All persistence, object storage, registry resolution, execution dispatch, and ACT credit settlement go through the 5 traits in `crates/core-traits`: `SessionStore`, `BlobStore`, `OciRegistryClient`, `RunDispatcher`, and `PaymentGateway`.
2. **Universal Plugin Contract (`Plugin = Module = Pipeline`):**
   - Every plugin backend is 1 OCI container (`plugin.toml` + `Dockerfile` + `SKILL.md`) reading `/in/params.json` + `/in/data/*.parquet` (`ro`) and writing `/out/result.json` + `/out/artifacts/*` (`rw`).
   - Optional custom UIs live in `ui/` (Vite build) and run inside `<iframe sandbox="allow-scripts">` communicating strictly over `postMessage` JSON-RPC 2.0 via `@biocompass/plugin-sdk`.
3. **3-Level DuckLake Catalog Access & Sealed Snapshots (`crates/data-engine`):**
   - Scientific catalogs use schema-generic Arrow `RecordBatch` tables and sealed SHA-256 snapshots (`dataset_digest`) with a verify-once content-addressed shard cache and offline-bundled DuckDB extensions.
   - Access is strictly 3-level (`none`, `compute_only`, `rows` under `row_scope`, `daily_row_budget`, and `allow_local_export`) with zero `***` column masking; callers query through a typed `QueryAst` (never raw SQL), and only digest-approved curated modules may full-scan a `compute_only` or protected catalog.
   - Containers never receive database or S3 credentials; `data-engine` materializes scoped or full-scan Parquet slices directly into `/in/data/` without silent row truncation.
4. **Hardened Container Execution (`crates/oci-runner`):**
   - Prefer rootless `podman` on Linux (falling back to standard `docker` with a startup warning when rootless Podman is unavailable) and VM-isolated `docker` / `podman` on macOS/Windows, enforcing `--user 65532:65532`, `--read-only`, `--cap-drop=ALL`, `--security-opt=no-new-privileges`, `--pids-limit`, bounded `/tmp` tmpfs (`noexec,nosuid`), `/in:ro`, `/out:rw`, and enforced Offer input/output/runtime/memory limits.
   - Cloud, marketplace, and edge worker runs strictly enforce `--network=none` and `@sha256`-pinned image digests; `local` workstation mode permits declared `allow_domains` (`--network=bridge` with an explicit warning) and local dev/bundled builds for fast iteration.
5. **Verifiable Froglet Receipts (`crates/node-runtime`):**
   - Every completed run signs a `froglet/v1` RFC 8785 JCS + `secp256k1` BIP340 Schnorr artifact chain (`Descriptor -> Offer -> Quote -> Deal -> Receipt` using `settlement_method = "none"` on local laptop runs and `settlement_method = "operator_ledger.v1"` on cloud/Froglet-node runs) validated via `froglet-verify`.

## Commands

```sh
npm install                  # Install workspace dependencies (packages/plugin-sdk, web)
npm run build                # Build @biocompass/plugin-sdk and web/dist
npm run typecheck            # TypeScript strict check across SDK and web
cargo test --workspace       # Run unit & integration tests across all Rust crates
cargo clippy --workspace     # Lint Rust workspace
cargo build --bin biocompass # Build single binary with embedded web/dist
cargo run --bin biocompass -- up --port 8080
```
