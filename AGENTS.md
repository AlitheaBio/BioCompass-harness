# AGENTS.md

BioCompass (`AlitheaBio/BioCompass-harness`) is a cloud-agnostic Rust 2024 + Vite 8 / React 19 computational biology agent harness and OCI plugin execution workbench built on `froglet-protocol`.

## Hard Repository Boundaries

- Never modify legacy or service reference repositories (`HLA-Compass-platform`, `froglet-services`); `AlitheaBio/froglet` is consumed by pinned commit SHA and receives only the approved v2 `operator_ledger.v1` settlement addition.
- Never copy private cloud planning documents (`MILESTONE_C_PLAN.md`) into this public repository.
- Keep `CLAUDE.md` symlinked to `AGENTS.md`.

## Core Architectural Invariants

1. **Zero Duct Tape & Cloud-Agnostic Traits (`crates/core-traits`, `crates/mcp-core`):**
   - Domain crates (`plugin-spec`, `data-engine`, `oci-runner`, `node-runtime`, `agent-core`, `mcp-core`) never branch on cloud vendor SDKs or `if is_cloud`.
   - All persistence, object storage, registry resolution, execution dispatch, and ACT/EUR settlement go through the 5 traits in `crates/core-traits`: `SessionStore`, `BlobStore`, `OciRegistryClient`, `RunDispatcher`, and `PaymentGateway`.
   - `crates/mcp-core` provides the shared JSON-RPC 2.0 MCP envelope, protocol-version negotiation, and `ToolHost` trait; `compass-hub` never depends on `biocompass-agent-core`.
2. **Universal Plugin Contract (`Plugin = Module = Pipeline`):**
   - Every plugin backend is 1 OCI container (`plugin.toml` + `Dockerfile` + `SKILL.md`) reading `/in/params.json`, `/in/data/*.parquet`, and verified chained run inputs under `/in/artifacts/<alias>/*` (`ro`), and writing `/out/result.json` + `/out/artifacts/*` (`rw`).
   - Live catalog queries and depth-1 child-module calls use the reserved `.bc` file mailbox (`/out/.bc/requests/`, `/in/.bc/replies/` via atomic temp-file + same-directory rename, polled with `O_NOFOLLOW|O_NONBLOCK`, `fstat`, and bounded reads). Modules may invoke only declared `runtime_queries` and declared `child_modules`: an explicit list of module ids, or `"caller"` (the module may start any module the initiating principal could start directly; the hub authorises every child as that principal: the parent's agent credential, otherwise the person who confirmed the parent). The value is part of the signed terms. Depth stays one; one child is in flight per parent; the worker wall clock pauses while a child runs, capped by signed `max_child_wait_sec`; a human parent's children are bounded by its signed `max_child_spend_milli_act` (ledger-enforced), a credential parent's by the credential ceilings. Abandoned children and children of an ended parent are canceled and refunded.
   - Optional custom UIs live in `ui/` (Vite build) and run inside `<iframe sandbox="allow-scripts">` communicating strictly over `postMessage` JSON-RPC 2.0 via `@biocompass/plugin-sdk`, preserving `SEC-17` sandbox CSP and viewer-bound tickets.
3. **Verified UI Cache & Unified Module UX (Track U):**
   - Module UI bundles are content-addressed, signed artifacts (`ui_bundle_hash` exposed as a single archive at `/v1/artifacts/<hash>`). Every workspace maintains a verified UI cache (`bins/biocompass/src/ui_cache.rs`), pre-fetches bundles on listing/sync, serves pre-compressed assets from cache (< 1 s warm and cold), and fails closed on hash tampering.
   - Module listings separate `source` (`workspace`, `org_cloud`, `network`) from `runs_in` (`local`, `cloud`, `network`) with consistent `Open`, `Install`, `Remove`, and `Add` semantics; the local client supports in-app hub switching (Alithea cloud, customer cloud URL, or offline).
4. **3-Level DuckLake Catalog Access & Sealed Snapshots (`crates/data-engine`):**
   - Scientific catalogs use schema-generic Arrow `RecordBatch` tables and sealed SHA-256 snapshots (`dataset_digest`) with a verify-once content-addressed shard cache and offline-bundled DuckDB extensions.
   - Access is strictly 3-level (`none`, `compute_only`, `rows` under `row_scope`, `daily_row_budget`, and `allow_local_export`) with zero `***` column masking; callers query through a typed `QueryAst` (never raw SQL), and only digest-approved curated modules may full-scan a `compute_only` or protected catalog.
   - Containers never receive database or S3 credentials; `data-engine` materializes scoped or full-scan Parquet slices directly into `/in/data/` without silent row truncation.
5. **Hardened Container Execution (`crates/oci-runner` `ContainerRunner` Seam):**
   - Execution goes through the single `ContainerRunner` trait and shared `supervise()` loop.
   - Hosted Kubernetes runs execute as unprivileged, network-isolated Run Pods under Pod Security `restricted` (`biocompass-kube-runner`) with zero Docker-in-Docker sidecars and zero host engine sockets.
   - Local workstation mode keeps Docker/Podman behind the same `ContainerRunner` interface (preferring rootless `podman` on Linux with fallback warning to `docker`, and VM-isolated `docker`/`podman` on macOS/Windows), enforcing `--user 65532:65532`, `--read-only`, `--cap-drop=ALL`, `--security-opt=no-new-privileges`, `--pids-limit`, bounded `/tmp` tmpfs (`noexec,nosuid`), `/in:ro`, `/out:rw`, and enforced Offer input/output/runtime/memory limits.
   - Cloud, marketplace, and edge worker runs strictly enforce `--network=none` and `@sha256`-pinned image digests; `local` workstation mode permits declared `allow_domains` (`--network=bridge` with an explicit warning) and clean-clone bundled `Dockerfile` fallback (`R11`).
6. **Verifiable Froglet Provenance Receipts (`crates/node-runtime`, `bins/biocompass`):**
   - Every completed run signs a `froglet/v1` RFC 8785 JCS + `secp256k1` BIP340 Schnorr artifact chain (`Descriptor -> Offer -> Quote -> Deal -> Receipt` using `settlement_method = "none"` on local laptop runs and `settlement_method = "operator_ledger.v1"` on cloud/Froglet-node runs) validated via `froglet-verify`.
   - Run-to-run provenance links live exclusively in signed `extension_refs` (`bc.input.v1:<sha256>:<source_receipt_hash>`, `bc.parent.v1:<parent_deal_hash>`, `bc.child.v1:<child_receipt_hash>`) with upstream `froglet-protocol` types unchanged; canonical workload hashes omit empty `inputs` for full backward compatibility.
   - Exported `bc.provenance.v1` bundles (five chain artifacts, manifest preimage, and inputs per run; zero `params`) are verified offline via `biocompass verify-receipt` for cryptographic validity, link integrity, acyclicity, and missing ancestors.

## Commands

```sh
npm install                  # Install workspace dependencies (packages/plugin-sdk, web)
npm run build                # Build @biocompass/plugin-sdk and web/dist
npm run typecheck            # TypeScript strict check across SDK and web
npm run test:unit            # Run web/SDK unit tests
cargo test --workspace       # Run unit & integration tests across all Rust crates
cargo clippy --workspace --all-targets -- -D warnings
cargo build --bin biocompass # Build single binary with embedded web/dist
cargo run --bin biocompass -- up --port 8080
```
