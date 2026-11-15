# BioCompass

**BioCompass** is Alithea Bio's cloud-agnostic computational biology agent, data catalog explorer, and OCI container execution harness powered by the [Froglet](https://github.com/armanas/froglet) verifiable compute protocol.

Every scientific module or pipeline in BioCompass is **one standard OCI container** (`/in/params.json` + `/in/data/*.parquet` → `Dockerfile` → `/out/result.json` + `/out/artifacts/*`) paired with an optional sandboxed Vite 8 UI (`@biocompass/plugin-sdk`). Every run produces an offline-verifiable **Froglet BIP340 Schnorr `Receipt`** binding input parameters, SHA-256 dataset digests, container digest, and result hashes.

## Architecture

```text
BioCompass-harness/
├── Cargo.toml                  # Rust 2024 workspace
├── crates/
│   ├── core-traits/            # 5 cloud-agnostic traits (SessionStore, BlobStore, OciRegistryClient, RunDispatcher, PaymentGateway) + SQLite & POSIX impls
│   ├── plugin-spec/            # plugin.toml manifest parser, SKILL.md loader, JSON Schema validator, 4 starter generators
│   ├── data-engine/            # Embedded DuckDB over immutable Parquet + catalog.json SHA-256 + Grant row/*** sequence masking SQL views
│   ├── oci-runner/             # Rootless Podman (Linux) & Docker/Podman VM (macOS/Windows) hardened /in:ro -> /out:rw supervisor
│   ├── node-runtime/           # Embeddable Froglet execution engine + Stage-In/Out + 5-artifact Schnorr chain signer & verifier
│   └── agent-core/             # Streaming Rust LLM SSE loop, SKILL.md injection, >12k-token output spill, durable session events
├── bins/
│   └── biocompass/             # Single CLI binary (`biocompass up`, `run`, `init-plugin`, `verify-receipt`, `--mode cloud-web`)
├── packages/
│   └── plugin-sdk/             # @biocompass/plugin-sdk (<iframe sandbox="allow-scripts"> postMessage JSON-RPC 2.0 bridge)
├── web/                        # Single Vite 8 + React 19 SPA (Workbench, Grant-Masked Catalog Explorer, Sandboxed Plugin Host, Builder Studio)
└── templates/                  # Reference starter plugins (python-uv, r-bioc, nextflow, rust-cli)
```

## Quick Start

```sh
# 1. Build @biocompass/plugin-sdk and the Vite 8 + React 19 SPA
npm install
npm run build

# 2. Run the Rust workspace test suite & build the single embedded binary
cargo test --workspace
cargo build --bin biocompass

# 3. Launch BioCompass locally on http://127.0.0.1:8080
cargo run --bin biocompass -- up --port 8080
```

## Cloud-Agnostic Deployment

Domain crates depend exclusively on the 5 traits in `biocompass-core-traits` (`SessionStore`, `BlobStore`, `OciRegistryClient`, `RunDispatcher`, `PaymentGateway`) and speak 100% standard open protocols:
- **Local Mode (`biocompass up`):** SQLite WAL (`state.sqlite`), POSIX filesystem (`~/.biocompass/store`), local Podman/Docker, and embedded in-process `NodeRuntime`.
- **Cloud Mode (`biocompass --mode cloud-web` + `compass-hub`):** PostgreSQL 16, S3 REST v4 (MinIO, Ceph, Scaleway, Exoscale, Hetzner, OVH), OCI Distribution v1.1 (`/v2/`), and remote rootless Podman `froglet-node` pools.

## License

Licensed under the [MIT License](LICENSE).
