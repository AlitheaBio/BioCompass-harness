# AGENTS.md

BioCompass (`AlitheaBio/BioCompass-harness`) is a cloud-agnostic Rust 2024 + Vite 8 / React 19 computational biology agent harness and OCI plugin execution workbench built on `froglet-protocol`.

## Hard Repository Boundaries

- Never modify sibling reference repositories (`HLA-Compass-platform`, `froglet`, `froglet-services`); consume `froglet-protocol` and `froglet-verify` as read-only dependencies.
- Keep `CLAUDE.md` symlinked to `AGENTS.md`.

## Core Architectural Invariants

1. **Zero Duct Tape & Cloud-Agnostic Traits (`crates/core-traits`):**
   - Domain crates (`plugin-spec`, `data-engine`, `oci-runner`, `node-runtime`, `agent-core`) never branch on cloud vendor SDKs or `if is_cloud`.
   - All persistence, object storage, registry resolution, execution dispatch, and ACT credit settlement go through the 5 traits in `crates/core-traits`: `SessionStore`, `BlobStore`, `OciRegistryClient`, `RunDispatcher`, and `PaymentGateway`.
2. **Universal Plugin Contract (`Plugin = Module = Pipeline`):**
   - Every plugin backend is 1 OCI container (`plugin.toml` + `Dockerfile` + `SKILL.md`) reading `/in/params.json` + `/in/data/*.parquet` (`ro`) and writing `/out/result.json` + `/out/artifacts/*` (`rw`).
   - Optional custom UIs live in `ui/` (Vite build) and run inside `<iframe sandbox="allow-scripts">` communicating strictly over `postMessage` JSON-RPC 2.0 via `@biocompass/plugin-sdk`.
3. **DuckDB Grant Enforcement (`crates/data-engine`):**
   - Scientific datasets are immutable Parquet folders verified against `catalog.json` SHA-256 digests before query or stage-in.
   - Row-level allowlists (`allowed_organs`, `allowed_conditions`) and column-level `***` peptide sequence masking are compiled into DuckDB `TEMP VIEW`s; callers query through typed `CatalogQuery` specs rather than raw SQL strings to prevent regex/timing side-channels on masked columns.
   - Containers never receive database or S3 credentials; `data-engine` materializes pre-masked Parquet slices directly into `/in/data/`.
4. **Hardened Container Execution (`crates/oci-runner`):**
   - Enforce rootless `podman` on Linux and VM-isolated `podman` / `docker` on macOS/Windows with `--user 65532:65532`, `--read-only`, `--cap-drop=ALL`, `--security-opt=no-new-privileges`, `--network=none` by default, `--pids-limit`, bounded `/tmp` tmpfs (`noexec,nosuid`), `/in:ro`, and `/out:rw`.
5. **Verifiable Froglet Receipts (`crates/node-runtime`):**
   - Every completed run signs a `froglet/v1` RFC 8785 JCS + `secp256k1` BIP340 Schnorr artifact chain (`Descriptor -> Offer -> Quote -> Deal -> Receipt` with `settlement_method = "none"`) validated via `froglet-verify`.

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
