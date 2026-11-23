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
# 1. Bootstrap SHA-256-verified offline DuckDB extensions for your host platform
./scripts/install-duckdb-extensions.sh

# 2. Build @biocompass/plugin-sdk and the Vite 8 + React 19 SPA
npm install
npm run build

# 3. Run the Rust workspace test suite & build the single embedded binary
cargo test --workspace
cargo build --bin biocompass

# 4. Launch BioCompass locally on http://127.0.0.1:8080
cargo run --bin biocompass -- up --port 8080
```

### Import and version private datasets locally

When `biocompass up` binds to a loopback address in `local` mode, it writes a single-use **Local dataset owner** link to `<data-dir>/owner-link.txt` (valid for 10 minutes; grants an 8-hour owner session scoped to that loopback origin). Open the link in your browser—or exchange its `#owner=` ticket via `POST /api/v1/local/owner-session` and send `X-BioCompass-Local-Owner`—to run installed air-gapped dataset importers and preview/publish signed dataset version changes in Data Explorer while keeping Froglet private keys on the laptop.

### Retrieve hosted results in a local workspace

Start the personal workspace with the hub's HTTPS base URL in `COMPASS_HUB_URL`. For a hub reached over loopback, HTTP is supported. From a second terminal, use your organization's `COMPASS_API_KEY` or `COMPASS_BEARER_TOKEN` for the following commands.

```sh
biocompass workspace prepare --plugin "$PLUGIN_ID"
# Review the price and set RUN_ID to the run ID in the quote.
biocompass workspace confirm "$RUN_ID"

# After reconnecting, start the same workspace and collect the result.
biocompass workspace sync
biocompass workspace runs "$RUN_ID"
biocompass workspace result "$RUN_ID" --artifact "$ARTIFACT_NAME" > result.bin
```

The worker can finish while the workspace is stopped. Repeating sync picks up outstanding deliveries without running the module again. A locally saved result is available to the owning OS account even with both services offline. Use the hub origin, organization and subject from `biocompass workspace identity` together with the original data directory.

```sh
biocompass workspace --data-dir "$DATA_DIR" --hub-url "$HUB_ORIGIN" \
  --org "$ORG_ID" --subject "$SUBJECT" result "$RUN_ID" > result.json
```

## Cloud-Agnostic Deployment

Domain crates depend exclusively on the 5 traits in `biocompass-core-traits` (`SessionStore`, `BlobStore`, `OciRegistryClient`, `RunDispatcher`, `PaymentGateway`) and speak 100% standard open protocols:
- **Local Mode (`biocompass up`):** SQLite WAL (`state.sqlite`), POSIX filesystem (`~/.biocompass/store`), local Podman/Docker, and embedded in-process `NodeRuntime`.
- **Cloud Mode (`biocompass --mode cloud-web` + `compass-hub`):** PostgreSQL 16, S3 REST v4 (MinIO, Ceph, Scaleway, Exoscale, Hetzner, OVH), OCI Distribution v1.1 (`/v2/`), and remote rootless Podman `froglet-node` pools.

## License

Licensed under the [MIT License](LICENSE).

## Local app release channel

Open **Settings → Appearance → Local app** to see releases from the maintainer’s signed channel. On a local installation, the workbench also checks at startup and every day while it stays open. A verified newer release adds an update indicator to Settings. An installation can continue using BioCompass with the channel offline or unconfigured. The native app reports its compiled version and platform. The cloud workbench uses browser platform information when available and lets the scientist select a platform explicitly.

Set `BIOCOMPASS_RELEASE_CHANNEL` to the HTTPS URL of `latest.json` and `BIOCOMPASS_RELEASE_PUBLIC_KEY` to the base64 public-key line from a trusted Minisign key. Set these on the BioCompass gateway, including the local installation if it should receive update notices. For laptop tests, an HTTP URL on a numeric loopback address is also accepted. Provision the public key independently of the release host. Keep the private signing key with the publisher.

To publish a release:

1. Build and check the actual versioned binaries or packages for each supported system. Upload immutable artifact filenames to static storage on the same origin as `latest.json`. The origin must serve direct `HEAD` and `GET` requests.
2. Sign each file with `minisign -Sm <artifact> -s <release-private-key>`. Use Tauri's static update metadata shape for `latest.json`: a stable semantic `version`, plaintext `notes`, and a `platforms` map. Each key, such as `darwin-aarch64`, maps to `{ "url": "...", "signature": "..." }`. The signature is the complete Minisign signature text or its base64 encoding. Publish only targets whose artifacts exist.
3. Sign the completed metadata with `minisign -Sm latest.json -s <release-private-key>`. Publish `latest.json` and `latest.json.minisig` together after the assets. The workbench verifies that signature before displaying the version and release notes. A download rechecks the metadata the scientist selected, verifies the entire artifact in a temporary file, and offers the verified file to the browser.

“Download verified update” saves a file. To use a portable archive, extract it, stop the running app and start the new binary with the original `--data-dir`. An OS package uses its normal installer. Workspace and account files are separate from the release artifact. The gateway does not replace or execute installed binaries. A failed or canceled download can be retried. Ports, signing keys, release storage and installers for real platforms are deployment inputs.

A channel can also provide a compatible customer worker. Add a separate entry to the signed `platforms` map named `froglet-node-<platform>`, such as `froglet-node-darwin-aarch64`. Include its own stable `version`, exact `url`, artifact `signature` and optional worker `notes`. This entry contains the BioCompass-compatible `froglet-node` binary and can have a different version from the local app. The authenticated customer hub checks the user's role for discovery and download. Organization owners and admins can download the worker and see connection guidance. Keep the generated node key and persistent state on the customer machine. A site operator enrolls compute-pool workers; an organization administrator can enroll a private-data worker with its real signing public key.
