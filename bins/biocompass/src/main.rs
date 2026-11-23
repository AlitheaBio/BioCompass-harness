//! `biocompass` single-binary CLI (`up`, `init-plugin`, `publish-plugin`, `catalog import`, `run`, `verify-receipt`).

mod publish_ui;
mod workspace_cli;

use biocompass::server::build_router;
use biocompass::state::{AppState, DeploymentMode, local_catalog_dir};
use biocompass_core_traits::{
    CatalogId, CompassError, CompassResult, IngestFormat, IngestMode, PluginId, PrepareRunRequest,
    RunStatus,
};
use biocompass_data_engine::{
    DuckDbCatalogEngine, QuarantineIngestSpec, validate_and_normalize_quarantine_dir,
};
use biocompass_oci_runner::{EngineKind, detect_container_engine};
use biocompass_plugin_spec::{
    LoadedPlugin, PluginManifest, SkillDoc, TemplateKind, write_template_to_dir,
};
use clap::{Parser, Subcommand};
use froglet_verify::{extract_documents, validate_chain_documents};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(
    name = "biocompass",
    version,
    about = "Cloud-agnostic computational biology agent harness & Froglet OCI plugin workbench"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Submit hosted work and retrieve results in the installed workspace.
    Workspace(workspace_cli::Workspace),
    /// Start the BioCompass HTTP/SSE server and embedded Vite/React workbench.
    Up {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 8080)]
        port: u16,
        #[arg(long)]
        data_dir: Option<PathBuf>,
        #[arg(long, default_value = "local")]
        mode: String,
        #[arg(long, default_value_t = false)]
        allow_local_dev_tags: bool,
    },
    /// Scaffold a new OCI container plugin from one of the 4 starter templates.
    InitPlugin {
        #[arg(long, default_value = "python-uv")]
        template: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value_t = false)]
        with_ui: bool,
        #[arg(long)]
        out: PathBuf,
    },
    /// Validate `plugin.toml`, push/verify `@sha256` OCI image, upload UI bundle, and register on Compass Hub (§6.4).
    PublishPlugin {
        /// Plugin directory containing `plugin.toml`, optional `SKILL.md`, `Dockerfile`, and `ui/dist/`.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Compass Hub or Cloud Web base URL.
        #[arg(long, default_value = "http://127.0.0.1:18100")]
        hub_url: String,
        /// Developer/Admin/Owner API key (`bc_live_...`).
        #[arg(long)]
        api_key: Option<String>,
        /// Optional Bearer token alternative to `--api-key`.
        #[arg(long)]
        bearer_token: Option<String>,
        /// Optional Cookie header alternative (`bc_session=...`).
        #[arg(long)]
        cookie: Option<String>,
        /// Target visibility (`org_private`, `public_pending_review`, `public_approved`, or `rejected`).
        #[arg(long, default_value = "org_private")]
        visibility: String,
        /// Optional OCI registry host override (e.g. `127.0.0.1:15500`) used when building & pushing.
        #[arg(long)]
        registry: Option<String>,
        /// Build `Dockerfile` in `--dir` and push to OCI registry before registering.
        #[arg(long, default_value_t = false)]
        build_and_push: bool,
    },
    /// Execute a two-step `Prepare -> Confirm` container run from the CLI and emit a signed Froglet chain.
    Run {
        /// Path to a plugin directory containing `plugin.toml`, or a registered `<org>/<slug>` ID.
        #[arg(long)]
        plugin: String,
        /// Exact published plugin version to run. Omit to use the current version.
        #[arg(long)]
        plugin_version: Option<String>,
        /// Optional Offer ID declared under `[[offers]]` in `plugin.toml`.
        #[arg(long)]
        offer_id: Option<String>,
        /// JSON object string passed into `/in/params.json`.
        #[arg(long, default_value = "{}")]
        params: String,
        /// Dataset grant preset (`full`, `partner`, or `none`).
        #[arg(long, default_value = "full")]
        grant: String,
        #[arg(long)]
        data_dir: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        allow_local_dev_tags: bool,
        /// Optional path to write the 5-artifact `froglet/v1` signed chain JSON.
        #[arg(long)]
        receipt_out: Option<PathBuf>,
    },
    /// Offline-verify a signed `froglet/v1` artifact chain JSON file without network or node access.
    VerifyReceipt {
        /// Path to the Froglet chain JSON file.
        file: PathBuf,
        #[arg(long)]
        now: Option<i64>,
    },
    /// Scientific DuckLake catalog management (`import` for Parquet/CSV/TSV locally or via cloud S3 quarantine, §7.2).
    Catalog {
        #[command(subcommand)]
        action: CatalogCommands,
    },
}

#[derive(Debug, Subcommand)]
enum CatalogCommands {
    /// Import a Parquet, CSV, or TSV table (`append`, `replace`, or `edit`) into a local or cloud DuckLake catalog.
    Import {
        /// Path to the input `.parquet`, `.csv`, or `.tsv` file.
        #[arg(long)]
        file: PathBuf,
        /// Target `<namespace>/<name>` catalog ID.
        #[arg(long)]
        catalog_id: String,
        /// Target table name inside the catalog.
        #[arg(long, default_value = "records")]
        table: String,
        /// Commit mode (`append`, `replace`, or `edit`).
        #[arg(long, default_value = "replace")]
        mode: String,
        /// Input file format (`parquet`, `csv`, or `tsv`; inferred from `--file` extension when omitted).
        #[arg(long)]
        format: Option<String>,
        /// Optional comma-separated primary-key column names (`e.g. sample_id,peptide_id`).
        #[arg(long)]
        primary_key: Option<String>,
        /// Optional human-readable catalog display name (when creating a new catalog).
        #[arg(long)]
        name: Option<String>,
        /// Optional catalog description.
        #[arg(long)]
        description: Option<String>,
        /// Optional organism annotation.
        #[arg(long, default_value = "Homo sapiens")]
        organism: String,
        /// Optional commit message recorded on the sealed DuckLake snapshot.
        #[arg(long)]
        commit_message: Option<String>,
        /// Local data directory (`~/.biocompass`) used when `--hub-url` is omitted.
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Compass Hub or Cloud Web URL; when set, runs direct-to-S3 quarantine multipart upload + Stage 1/2 ingest.
        #[arg(long)]
        hub_url: Option<String>,
        /// Developer/Admin/Owner API key (`bc_live_...`) for cloud mode.
        #[arg(long)]
        api_key: Option<String>,
        /// Optional Bearer token alternative for cloud mode.
        #[arg(long)]
        bearer_token: Option<String>,
        /// Optional Cookie header (`bc_session=...`) for cloud mode.
        #[arg(long)]
        cookie: Option<String>,
        /// Multipart chunk size in MiB for cloud direct-to-S3 uploads (`>= 5` MiB for multi-part S3).
        #[arg(long, default_value_t = 8)]
        part_size_mb: u64,
    },
}

fn default_data_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME")
        && !home.trim().is_empty()
    {
        return PathBuf::from(home).join(".biocompass");
    }
    PathBuf::from(".biocompass")
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "biocompass=info,warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match run_cli(cli).await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(1)
        }
    }
}

async fn run_cli(cli: Cli) -> CompassResult<ExitCode> {
    match cli.command {
        Commands::Workspace(workspace) => workspace.execute().await,
        Commands::Up {
            host,
            port,
            data_dir,
            mode,
            allow_local_dev_tags,
        } => {
            let deploy_mode = DeploymentMode::parse(&mode)?;
            let dir = data_dir.unwrap_or_else(default_data_dir);
            let addr: SocketAddr = format!("{host}:{port}")
                .parse()
                .map_err(|e| CompassError::Validation(format!("invalid listen address: {e}")))?;

            let installed_workspace = if deploy_mode == DeploymentMode::Local && addr.ip().is_loopback() {
                std::env::var("COMPASS_HUB_URL")
                    .ok()
                    .map(|url| biocompass::workspace::InstalledWorkspace::open(&dir, &url))
                    .transpose()?
                    .map(std::sync::Arc::new)
            } else {
                None
            };
            let mut state = AppState::bootstrap(&dir, deploy_mode, allow_local_dev_tags).await?;
            if let Some(workspace) = &installed_workspace {
                workspace.start_sync();
            }
            state.installed_workspace = installed_workspace;

            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .map_err(|e| CompassError::Internal(format!("bind {addr} failed: {e}")))?;
            eprintln!(
                "BioCompass v{} ({}) listening on http://{} [node={}]",
                env!("CARGO_PKG_VERSION"),
                deploy_mode.as_str(),
                addr,
                state.registry.node_id()
            );

            let bound = listener.local_addr().map_err(|e| CompassError::Storage(e.to_string()))?;
            state.set_listen_addr(bound);
            if let Some(link) = state.enable_local_owner(bound)? {
                use std::io::IsTerminal;
                if std::io::stderr().is_terminal() {
                    eprintln!("Local dataset owner (one use, 10 minutes): {link}");
                } else {
                    eprintln!("Local dataset owner link: {}", dir.join("owner-link.txt").display());
                }
            }
            let router = build_router(state);
            // Bind route state once so keep-alive connections share the same routing table.
            axum::serve(listener, router.into_make_service())
                .with_graceful_shutdown(shutdown_signal())
                .await
                .map_err(|e| CompassError::Internal(format!("http server error: {e}")))?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::InitPlugin {
            template,
            id,
            name,
            with_ui,
            out,
        } => {
            let kind = TemplateKind::parse(&template)?;
            let written = write_template_to_dir(kind, &id, &name, with_ui, &out)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "plugin_id": id,
                    "template": kind.as_str(),
                    "directory": out,
                    "files": written,
                }))
                .unwrap_or_default()
            );
            Ok(ExitCode::SUCCESS)
        }
        Commands::PublishPlugin {
            dir,
            hub_url,
            api_key,
            bearer_token,
            cookie,
            visibility,
            registry,
            build_and_push,
        } => {
            execute_publish_plugin(
                &dir,
                &hub_url,
                api_key.as_deref(),
                bearer_token.as_deref(),
                cookie.as_deref(),
                &visibility,
                registry.as_deref(),
                build_and_push,
            )
            .await
        }
        Commands::Run {
            plugin,
            plugin_version,
            offer_id,
            params,
            grant,
            data_dir,
            allow_local_dev_tags,
            receipt_out,
        } => {
            let dir = data_dir.unwrap_or_else(default_data_dir);
            let state = AppState::bootstrap(&dir, DeploymentMode::Local, allow_local_dev_tags).await?;
            biocompass_core_traits::validate_requested_plugin_version(plugin_version.as_deref())?;
            let params_val: Value = serde_json::from_str(&params)
                .map_err(|e| CompassError::Validation(format!("invalid --params JSON: {e}")))?;

            let plugin_path = Path::new(&plugin);
            let (plugin_id, plugin_version) = if plugin_path.is_dir() && plugin_path.join("plugin.toml").is_file() {
                let loaded = LoadedPlugin::load_from_dir(plugin_path, allow_local_dev_tags)?;
                if plugin_version.as_ref().is_some_and(|v| v != &loaded.manifest.version) {
                    return Err(CompassError::Validation(format!(
                        "requested plugin version does not match directory manifest version '{}'", loaded.manifest.version
                    )));
                }
                let selected = (loaded.manifest.id.clone(), Some(loaded.manifest.version.clone()));
                state.registry.register_plugin(loaded).await?;
                selected
            } else {
                (PluginId::new(plugin).map_err(CompassError::Validation)?, plugin_version)
            };

            let dataset_grant = state.resolve_grant(Some(&grant)).await?;
            let quote = state
                .dispatcher
                .prepare_run(PrepareRunRequest {
                    org_id: state.default_org_id.clone(),
                    session_id: None,
                    plugin_id,
                    plugin_version,
                    offer_id,
                    params: params_val,
                    dataset_grant,
                })
                .await?;

            let idem = format!("cli-{}", Uuid::new_v4().simple());
            let record = state.dispatcher.confirm_run(&quote.run_id, &idem).await?;

            if let Some(out_path) = receipt_out {
                if let Some(parent) = out_path.parent()
                    && !parent.as_os_str().is_empty()
                {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        CompassError::Storage(format!("create {}: {e}", parent.display()))
                    })?;
                }
                let chain_bytes = serde_json::to_vec_pretty(&record.froglet_chain)
                    .map_err(|e| CompassError::Internal(format!("serialize chain: {e}")))?;
                std::fs::write(&out_path, chain_bytes).map_err(|e| {
                    CompassError::Storage(format!("write {}: {e}", out_path.display()))
                })?;
            }

            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "run_id": record.run_id,
                    "plugin_id": record.plugin_id,
                    "status": record.status,
                    "receipt_verified": record.receipt_verified,
                    "result": record.result_json,
                    "artifacts": record.artifacts,
                    "error_message": record.error_message,
                }))
                .unwrap_or_default()
            );

            if record.status == RunStatus::Succeeded && record.receipt_verified {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::from(1))
            }
        }
        Commands::VerifyReceipt { file, now } => {
            let raw = std::fs::read_to_string(&file).map_err(|e| {
                CompassError::Storage(format!("read {}: {e}", file.display()))
            })?;
            let val: Value = serde_json::from_str(&raw)
                .map_err(|e| CompassError::Validation(format!("invalid JSON: {e}")))?;
            let docs = extract_documents(&val).map_err(CompassError::Protocol)?;
            let report = validate_chain_documents(&docs, now);
            println!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_default()
            );
            if report.valid {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::from(1))
            }
        }
        Commands::Catalog { action } => match action {
            CatalogCommands::Import {
                file,
                catalog_id,
                table,
                mode,
                format,
                primary_key,
                name,
                description,
                organism,
                commit_message,
                data_dir,
                hub_url,
                api_key,
                bearer_token,
                cookie,
                part_size_mb,
            } => {
                execute_catalog_import(
                    &file,
                    &catalog_id,
                    &table,
                    &mode,
                    format.as_deref(),
                    primary_key.as_deref(),
                    name.as_deref(),
                    description.as_deref(),
                    &organism,
                    commit_message.as_deref(),
                    data_dir,
                    hub_url.as_deref(),
                    api_key.as_deref(),
                    bearer_token.as_deref(),
                    cookie.as_deref(),
                    part_size_mb,
                )
                .await
            }
        },
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[derive(Debug, Clone, Serialize)]
struct IconFileUploadDto {
    path: String,
    size_bytes: u64,
    sha256: String,
    data_hex: String,
}

fn load_optional_plugin_icon(
    dir: &Path,
    manifest: &PluginManifest,
) -> CompassResult<Option<IconFileUploadDto>> {
    const MAX_ICON_BYTES: u64 = 256 * 1024;
    let Some(icon_rel) = manifest
        .icon
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    if icon_rel.contains('\\')
        || icon_rel.chars().any(char::is_control)
        || !icon_rel.ends_with(".svg")
        || !Path::new(icon_rel)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return Err(CompassError::Validation(format!(
            "plugin icon '{icon_rel}' must be a relative .svg file path without parent traversal"
        )));
    }
    let full = dir.join(icon_rel);
    let meta = std::fs::symlink_metadata(&full).map_err(|_| {
        CompassError::NotFound(format!(
            "plugin declares icon = '{icon_rel}', but {} was not found",
            full.display()
        ))
    })?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(CompassError::Validation(format!(
            "plugin icon '{}' must be a regular file, not a symlink",
            full.display()
        )));
    }
    if meta.len() == 0 || meta.len() > MAX_ICON_BYTES {
        return Err(CompassError::Validation(format!(
            "plugin icon '{}' must be 1..={MAX_ICON_BYTES} bytes",
            full.display()
        )));
    }
    let bytes = std::fs::read(&full)
        .map_err(|e| CompassError::Storage(format!("read {}: {e}", full.display())))?;
    Ok(Some(IconFileUploadDto {
        path: icon_rel.to_owned(),
        size_bytes: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&bytes)),
        data_hex: hex::encode(&bytes),
    }))
}

#[derive(Debug, Serialize)]
struct UiBundleFileUploadDto {
    path: String,
    content_type: String,
    data_hex: String,
}

#[derive(Deserialize)]
struct UiBundleUploadReceipt {
    ui_entry_url: String,
}

#[allow(clippy::too_many_arguments)]
async fn execute_publish_plugin(
    dir: &Path,
    hub_url: &str,
    api_key: Option<&str>,
    bearer_token: Option<&str>,
    cookie: Option<&str>,
    visibility: &str,
    registry_override: Option<&str>,
    build_and_push: bool,
) -> CompassResult<ExitCode> {
    let vis = visibility.trim();
    if !matches!(
        vis,
        "org_private" | "public_pending_review" | "public_approved" | "rejected"
    ) {
        return Err(CompassError::Validation(format!(
            "invalid --visibility '{vis}'; expected org_private, public_pending_review, public_approved, or rejected"
        )));
    }

    let resolved_api_key = api_key
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            std::env::var("COMPASS_API_KEY")
                .ok()
                .filter(|s| !s.trim().is_empty())
        });
    let resolved_bearer = bearer_token
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            std::env::var("COMPASS_BEARER_TOKEN")
                .ok()
                .filter(|s| !s.trim().is_empty())
        });
    let resolved_cookie = cookie
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            std::env::var("COMPASS_COOKIE")
                .ok()
                .filter(|s| !s.trim().is_empty())
        });
    if resolved_api_key.is_none() && resolved_bearer.is_none() && resolved_cookie.is_none() {
        return Err(CompassError::AccessDenied(
            "publish-plugin requires --api-key (or COMPASS_API_KEY), --bearer-token, or --cookie".into(),
        ));
    }

    let manifest_path = dir.join("plugin.toml");
    if !manifest_path.is_file() {
        return Err(CompassError::NotFound(format!(
            "missing plugin.toml in {}",
            dir.display()
        )));
    }
    let raw_toml = std::fs::read_to_string(&manifest_path)
        .map_err(|e| CompassError::Storage(format!("read {}: {e}", manifest_path.display())))?;
    let mut manifest = PluginManifest::from_toml_unvalidated(&raw_toml)?;
    if !manifest.network.is_airgapped() {
        return Err(CompassError::AccessDenied(
            "cloud plugin publishing requires network = \"none\" in plugin.toml".into(),
        ));
    }

    let skill_path = dir.join("SKILL.md");
    let skill = if skill_path.is_file() {
        let raw_md = std::fs::read_to_string(&skill_path)
            .map_err(|e| CompassError::Storage(format!("read {}: {e}", skill_path.display())))?;
        Some(SkillDoc::parse(&raw_md))
    } else {
        None
    };

    let icon_upload = load_optional_plugin_icon(dir, &manifest)?;

    let mut ui_files = Vec::new();
    if let Some(ui_spec) = &manifest.ui {
        let entry_file = dir.join(&ui_spec.entry);
        if !entry_file.is_file() {
            return Err(CompassError::NotFound(format!(
                "plugin declares [ui] entry '{}', but {} was not found (build UI first)",
                ui_spec.entry,
                entry_file.display()
            )));
        }
        let ui_dist_dir = entry_file.parent().unwrap_or(dir);
        let mut total_bytes = 0;
        collect_ui_bundle_files(ui_dist_dir, ui_dist_dir, &mut total_bytes, &mut ui_files)?;
        publish_ui::validate_entry_assets(&entry_file, &ui_files)?;
        ui_files.sort_by(|a, b| a.path.cmp(&b.path));
    }

    let http = reqwest::Client::builder()
        .build()
        .map_err(|e| CompassError::Internal(format!("build HTTP client: {e}")))?;

    if build_and_push || (!manifest.image.contains("@sha256:") && registry_override.is_some()) {
        let engine = detect_container_engine().await?;
        let repo = if let Some(reg) = registry_override.map(str::trim).filter(|s| !s.is_empty()) {
            format!("{}/{}", reg.trim_end_matches('/'), manifest.id.as_str())
        } else {
            let base = manifest
                .image
                .split_once("@sha256:")
                .map(|(r, _)| r)
                .or_else(|| manifest.image.rsplit_once(':').map(|(r, _)| r))
                .unwrap_or(manifest.image.as_str());
            base.to_owned()
        };
        let tag_ref = format!("{repo}:{}", manifest.version);
        if build_and_push {
            let dockerfile = dir.join("Dockerfile");
            if !dockerfile.is_file() {
                return Err(CompassError::NotFound(format!(
                    "--build-and-push requested, but {} does not exist",
                    dockerfile.display()
                )));
            }
            let mut build_cmd = tokio::process::Command::new(&engine.binary_path);
            build_cmd.arg("build");
            if engine.kind == EngineKind::Docker {
                build_cmd.arg("--provenance=false");
            }
            build_cmd.arg("-t").arg(&tag_ref).arg(dir);
            let out = build_cmd.output().await.map_err(|e| {
                CompassError::ContainerEngine(format!("spawn container build failed: {e}"))
            })?;
            if !out.status.success() {
                return Err(CompassError::ContainerEngine(format!(
                    "container build failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
        }
        let push_out = tokio::process::Command::new(&engine.binary_path)
            .args(["push", &tag_ref])
            .output()
            .await
            .map_err(|e| CompassError::ContainerEngine(format!("spawn container push: {e}")))?;
        if !push_out.status.success() {
            return Err(CompassError::ContainerEngine(format!(
                "container push '{}' failed: {}",
                tag_ref,
                String::from_utf8_lossy(&push_out.stderr).trim()
            )));
        }
        let digest_hex = fetch_oci_manifest_sha256(&http, &repo, &manifest.version).await?;
        manifest.image = format!("{repo}@sha256:{digest_hex}");
    }

    // Strict validation: @sha256:<64-hex> digest required!
    let verified_ref = manifest.validate(false)?;
    let expected_digest = verified_ref
        .sha256_digest
        .as_deref()
        .ok_or_else(|| CompassError::Validation("missing @sha256 digest".into()))?;
    let remote_digest =
        fetch_oci_manifest_sha256(&http, &verified_ref.repository, &format!("sha256:{expected_digest}"))
            .await?;
    if remote_digest != expected_digest {
        return Err(CompassError::IntegrityViolation(format!(
            "OCI registry digest mismatch: expected sha256:{expected_digest}, got sha256:{remote_digest}"
        )));
    }

    let base_url = hub_url.trim_end_matches('/');
    let apply_auth = |mut req: reqwest::RequestBuilder| -> reqwest::RequestBuilder {
        if let Some(k) = &resolved_api_key {
            req = req.header("X-API-Key", k);
        }
        if let Some(b) = &resolved_bearer {
            req = req.header("Authorization", format!("Bearer {b}"));
        }
        if let Some(c) = &resolved_cookie {
            req = req.header("Cookie", c);
        }
        req
    };

    let mut reg_payload = json!({
        "manifest": manifest,
        "skill": skill,
        "visibility": vis,
    });
    if let Some(icon_dto) = &icon_upload {
        reg_payload["icon_file"] = json!(icon_dto);
    }
    let reg_resp = apply_auth(http.post(format!("{base_url}/api/v1/plugins")).json(&reg_payload))
    .send()
    .await
    .map_err(|e| CompassError::Internal(format!("POST /api/v1/plugins failed: {e}")))?;
    if !reg_resp.status().is_success() {
        let status = reg_resp.status();
        let body = reg_resp.text().await.unwrap_or_default();
        return Err(CompassError::AccessDenied(format!(
            "POST /api/v1/plugins returned HTTP {status}: {body}"
        )));
    }
    let reg_json: Value = reg_resp.json().await.unwrap_or_else(|_| json!({}));

    let mut ui_entry_url: Option<String> = None;
    if !ui_files.is_empty() {
        let ui_url = format!(
            "{base_url}/api/v1/plugins/{}/versions/{}/ui-bundle",
            manifest.id.as_str(),
            manifest.version
        );
        let mut ui_payload = json!({ "files": ui_files });
        if let Some(icon_dto) = &icon_upload {
            ui_payload["icon_file"] = json!(icon_dto);
        }
        let ui_resp = apply_auth(http.put(&ui_url).json(&ui_payload))
            .send()
            .await
            .map_err(|e| CompassError::Internal(format!("PUT ui-bundle failed: {e}")))?;
        if !ui_resp.status().is_success() {
            let status = ui_resp.status();
            let body = ui_resp.text().await.unwrap_or_default();
            return Err(CompassError::Storage(format!(
                "PUT ui-bundle returned HTTP {status}: {body}"
            )));
        }
        let receipt: UiBundleUploadReceipt = ui_resp.json().await.map_err(|error| {
            CompassError::Protocol(format!("invalid UI upload receipt from hub: {error}"))
        })?;
        ui_entry_url = Some(receipt.ui_entry_url);
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "plugin_id": manifest.id,
            "version": manifest.version,
            "image": manifest.image,
            "image_digest": format!("sha256:{expected_digest}"),
            "visibility": reg_json.get("visibility").and_then(Value::as_str).unwrap_or(vis),
            "ui_files_uploaded": ui_files.len(),
            "ui_entry_url": ui_entry_url,
            "icon_uploaded": icon_upload.is_some(),
        }))
        .unwrap_or_default()
    );
    Ok(ExitCode::SUCCESS)
}

async fn fetch_oci_manifest_sha256(
    http: &reqwest::Client,
    repository: &str,
    reference: &str,
) -> CompassResult<String> {
    let (host, repo_path) = repository.split_once('/').ok_or_else(|| {
        CompassError::Validation(format!(
            "OCI repository '{repository}' must include registry host and repo path"
        ))
    })?;
    let is_local = host.starts_with("127.0.0.1")
        || host.starts_with("localhost")
        || host.starts_with("registry:");
    let scheme = if is_local { "http" } else { "https" };
    let url = format!("{scheme}://{host}/v2/{repo_path}/manifests/{reference}");
    let resp = http
        .get(&url)
        .header(
            "Accept",
            "application/vnd.oci.image.manifest.v1+json, application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.docker.distribution.manifest.list.v2+json",
        )
        .send()
        .await
        .map_err(|e| CompassError::ContainerEngine(format!("GET OCI manifest {url} failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(CompassError::NotFound(format!(
            "OCI manifest '{repository}:{reference}' not found in registry (HTTP {})",
            resp.status()
        )));
    }
    let header_digest = resp
        .headers()
        .get("docker-content-digest")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().strip_prefix("sha256:"))
        .map(str::to_ascii_lowercase);
    let body = resp
        .bytes()
        .await
        .map_err(|e| CompassError::ContainerEngine(format!("read OCI manifest body: {e}")))?;
    let computed = hex::encode(Sha256::digest(&body));
    if let Some(hdr) = header_digest
        && hdr != computed
    {
        return Err(CompassError::IntegrityViolation(format!(
            "OCI registry Docker-Content-Digest (sha256:{hdr}) != body SHA-256 (sha256:{computed})"
        )));
    }
    Ok(computed)
}

fn collect_ui_bundle_files(
    root: &Path,
    current: &Path,
    total_bytes: &mut u64,
    out: &mut Vec<UiBundleFileUploadDto>,
) -> CompassResult<()> {
    const MAX_UI_BUNDLE_BYTES: u64 = 32 * 1024 * 1024; // 32 MiB
    let entries = std::fs::read_dir(current)
        .map_err(|e| CompassError::Storage(format!("read UI dir {}: {e}", current.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| CompassError::Storage(format!("read UI entry: {e}")))?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| CompassError::Storage(format!("stat UI file {}: {e}", path.display())))?;
        if meta.file_type().is_symlink() {
            return Err(CompassError::IntegrityViolation(format!(
                "refusing symlink inside plugin UI bundle: {}",
                path.display()
            )));
        }
        if meta.is_dir() {
            collect_ui_bundle_files(root, &path, total_bytes, out)?;
        } else if meta.is_file() {
            *total_bytes = total_bytes.saturating_add(meta.len());
            if *total_bytes > MAX_UI_BUNDLE_BYTES {
                return Err(CompassError::Validation(format!(
                    "plugin UI bundle exceeds maximum size of {MAX_UI_BUNDLE_BYTES} bytes"
                )));
            }
            let rel = path
                .strip_prefix(root)
                .map_err(|e| CompassError::Internal(format!("strip UI prefix: {e}")))?
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = std::fs::read(&path).map_err(|e| {
                CompassError::Storage(format!("read UI file {}: {e}", path.display()))
            })?;
            let content_type = mime_guess::from_path(&rel)
                .first_or_octet_stream()
                .essence_str()
                .to_owned();
            out.push(UiBundleFileUploadDto {
                path: rel,
                content_type,
                data_hex: hex::encode(&bytes),
            });
        }
    }
    Ok(())
}

fn infer_ingest_format(file: &Path, explicit: Option<&str>) -> CompassResult<IngestFormat> {
    if let Some(raw) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return IngestFormat::parse(raw);
    }
    match file
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("parquet") | Some("pq") => Ok(IngestFormat::Parquet),
        Some("csv") => Ok(IngestFormat::Csv),
        Some("tsv") | Some("tab") => Ok(IngestFormat::Tsv),
        _ => Err(CompassError::Validation(format!(
            "cannot infer format from '{}'; pass --format parquet|csv|tsv explicitly",
            file.display()
        ))),
    }
}

#[derive(Debug, Deserialize)]
struct PresignedPartDto {
    part_number: u32,
    content_length: u64,
    url: String,
}

#[allow(clippy::too_many_arguments)]
async fn execute_catalog_import(
    file: &Path,
    catalog_id_raw: &str,
    table_raw: &str,
    mode_raw: &str,
    format_opt: Option<&str>,
    primary_key_opt: Option<&str>,
    name_opt: Option<&str>,
    description_opt: Option<&str>,
    organism_raw: &str,
    commit_message_opt: Option<&str>,
    data_dir_opt: Option<PathBuf>,
    hub_url_opt: Option<&str>,
    api_key_opt: Option<&str>,
    bearer_opt: Option<&str>,
    cookie_opt: Option<&str>,
    part_size_mb: u64,
) -> CompassResult<ExitCode> {
    if !file.is_file() {
        return Err(CompassError::NotFound(format!(
            "input file '{}' does not exist",
            file.display()
        )));
    }
    let catalog_id = CatalogId::new(catalog_id_raw.trim()).map_err(CompassError::Validation)?;
    catalog_id.ensure_legacy_writable()?;
    let mode = IngestMode::parse(mode_raw)?;
    let format = infer_ingest_format(file, format_opt)?;
    let table_name = {
        let t = table_raw.trim();
        if t.is_empty() { "records" } else { t }
    };
    let pk_cols: Vec<String> = primary_key_opt
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let clean_fname = file
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("import.dat")
        .to_owned();

    // Cloud mode: direct-to-S3 quarantine multipart upload + Stage 1/2 airgapped ingest (§7.2).
    if let Some(hub_url) = hub_url_opt.map(str::trim).filter(|s| !s.is_empty()) {
        let resolved_api_key = api_key_opt
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                std::env::var("BIOCOMPASS_API_KEY")
                    .ok()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
            });
        let resolved_bearer = bearer_opt
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let resolved_cookie = cookie_opt
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        if resolved_api_key.is_none() && resolved_bearer.is_none() && resolved_cookie.is_none() {
            return Err(CompassError::AccessDenied(
                "UNAUTHENTICATED: pass --api-key (or BIOCOMPASS_API_KEY), --bearer-token, or --cookie for cloud catalog import"
                    .into(),
            ));
        }

        let file_bytes = std::fs::read(file)
            .map_err(|e| CompassError::Storage(format!("read {}: {e}", file.display())))?;
        if file_bytes.is_empty() {
            return Err(CompassError::Validation("input file is empty".into()));
        }

        // S3 Multipart requires intermediate parts >= 5 MiB; only the final part may be smaller.
        let chunk_bytes = (part_size_mb.max(5) * 1024 * 1024) as usize;
        let slices: Vec<&[u8]> = file_bytes.chunks(chunk_bytes).collect();
        let part_sizes: Vec<u64> = slices.iter().map(|s| s.len() as u64).collect();

        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| CompassError::Internal(format!("build HTTP client: {e}")))?;
        let base_url = hub_url.trim_end_matches('/');
        let apply_auth = |mut req: reqwest::RequestBuilder| -> reqwest::RequestBuilder {
            if let Some(k) = &resolved_api_key {
                req = req.header("X-API-Key", k);
            }
            if let Some(b) = &resolved_bearer {
                req = req.header("Authorization", format!("Bearer {b}"));
            }
            if let Some(c) = &resolved_cookie {
                req = req.header("Cookie", c);
            }
            req
        };

        let init_resp = apply_auth(
            http.post(format!("{base_url}/api/v1/uploads/quarantine/init"))
                .json(&json!({
                    "filename": clean_fname,
                    "format": format.as_str(),
                    "mode": mode.as_str(),
                    "target_catalog_id": catalog_id,
                    "target_table": table_name,
                    "catalog_name": name_opt,
                    "catalog_description": description_opt,
                    "organism": organism_raw,
                    "primary_key": pk_cols,
                    "part_sizes": part_sizes,
                })),
        )
        .send()
        .await
        .map_err(|e| CompassError::Internal(format!("POST quarantine/init failed: {e}")))?;
        if !init_resp.status().is_success() {
            let status = init_resp.status();
            let body = init_resp.text().await.unwrap_or_default();
            return Err(CompassError::AccessDenied(format!(
                "quarantine/init returned HTTP {status}: {body}"
            )));
        }
        let init_json: Value = init_resp
            .json()
            .await
            .map_err(|e| CompassError::Protocol(format!("decode quarantine/init JSON: {e}")))?;
        let upload_record_id = init_json
            .get("upload")
            .and_then(|u| u.get("upload_record_id"))
            .and_then(Value::as_str)
            .ok_or_else(|| CompassError::Protocol("missing upload_record_id".into()))?
            .to_owned();
        let presigned_parts: Vec<PresignedPartDto> = serde_json::from_value(
            init_json
                .get("parts")
                .cloned()
                .ok_or_else(|| CompassError::Protocol("missing presigned parts".into()))?,
        )
        .map_err(|e| CompassError::Protocol(format!("decode presigned parts: {e}")))?;

        let mut completed_parts = Vec::with_capacity(presigned_parts.len());
        for (idx, part) in presigned_parts.iter().enumerate() {
            let slice = slices.get(idx).ok_or_else(|| {
                CompassError::Internal("presigned part count mismatch".into())
            })?;
            if slice.len() as u64 != part.content_length {
                return Err(CompassError::Validation(format!(
                    "part {} byte length {} != signed content_length {}",
                    part.part_number,
                    slice.len(),
                    part.content_length
                )));
            }
            let put_resp = http
                .put(&part.url)
                .header(reqwest::header::CONTENT_LENGTH, part.content_length)
                .body(slice.to_vec())
                .send()
                .await
                .map_err(|e| {
                    CompassError::Storage(format!(
                        "direct S3 PUT part {} failed: {e}",
                        part.part_number
                    ))
                })?;
            if !put_resp.status().is_success() {
                let status = put_resp.status();
                let body = put_resp.text().await.unwrap_or_default();
                let _ = apply_auth(http.post(format!(
                    "{base_url}/api/v1/uploads/quarantine/{upload_record_id}/abort"
                )))
                .send()
                .await;
                return Err(CompassError::Storage(format!(
                    "direct S3 PUT part {} returned HTTP {status}: {body}",
                    part.part_number
                )));
            }
            let etag = put_resp
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    CompassError::Protocol(format!(
                        "S3 PUT part {} response missing ETag header",
                        part.part_number
                    ))
                })?
                .to_owned();
            completed_parts.push(json!({
                "part_number": part.part_number,
                "etag": etag,
            }));
        }

        let complete_resp = apply_auth(
            http.post(format!(
                "{base_url}/api/v1/uploads/quarantine/{upload_record_id}/complete"
            ))
            .json(&json!({ "parts": completed_parts })),
        )
        .send()
        .await
        .map_err(|e| CompassError::Internal(format!("POST quarantine/complete failed: {e}")))?;
        if !complete_resp.status().is_success() {
            let status = complete_resp.status();
            let body = complete_resp.text().await.unwrap_or_default();
            return Err(CompassError::AccessDenied(format!(
                "quarantine/complete returned HTTP {status}: {body}"
            )));
        }

        let ingest_resp = apply_auth(http.post(format!(
            "{base_url}/api/v1/uploads/quarantine/{upload_record_id}/ingest"
        )))
        .send()
        .await
        .map_err(|e| CompassError::Internal(format!("POST quarantine/ingest failed: {e}")))?;
        if !ingest_resp.status().is_success() {
            let status = ingest_resp.status();
            let body = ingest_resp.text().await.unwrap_or_default();
            return Err(CompassError::Validation(format!(
                "quarantine/ingest returned HTTP {status}: {body}"
            )));
        }
        let ingest_json: Value = ingest_resp
            .json()
            .await
            .map_err(|e| CompassError::Protocol(format!("decode quarantine/ingest JSON: {e}")))?;

        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "plane": "cloud",
                "upload_record_id": upload_record_id,
                "upload": ingest_json.get("upload"),
                "catalog": ingest_json.get("catalog"),
            }))
            .unwrap_or_default()
        );
        return Ok(ExitCode::SUCCESS);
    }

    // Local mode: stage into temporary quarantine dir, validate + normalize, and commit into local DuckLake.
    let root = data_dir_opt.unwrap_or_else(default_data_dir);
    let cat_dir = local_catalog_dir(&root, &catalog_id);
    let existing_schema = if cat_dir.join("catalog.json").is_file() {
        DuckDbCatalogEngine::open_verified(&cat_dir)
            .ok()
            .and_then(|eng| {
                eng.manifest()
                    .tables
                    .iter()
                    .find(|t| t.name == table_name)
                    .cloned()
            })
    } else {
        None
    };

    let stage_root = root
        .join("stages")
        .join(format!("cli-import-{}", Uuid::new_v4().simple()));
    let in_dir = stage_root.join("in");
    let q_dir = in_dir.join("quarantine");
    let out_dir = stage_root.join("out");
    std::fs::create_dir_all(&q_dir)
        .map_err(|e| CompassError::Storage(format!("create {}: {e}", q_dir.display())))?;
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| CompassError::Storage(format!("create {}: {e}", out_dir.display())))?;

    let spec = QuarantineIngestSpec {
        filename: clean_fname.clone(),
        format,
        mode,
        table_name: table_name.to_owned(),
        primary_key: pk_cols,
        expected_schema: existing_schema,
    };
    std::fs::write(
        in_dir.join("ingest_spec.json"),
        serde_json::to_vec_pretty(&spec)
            .map_err(|e| CompassError::Internal(format!("encode ingest_spec: {e}")))?,
    )
    .map_err(|e| CompassError::Storage(format!("write ingest_spec.json: {e}")))?;
    std::fs::copy(file, q_dir.join(&clean_fname))
        .map_err(|e| CompassError::Storage(format!("copy {}: {e}", file.display())))?;

    let cname = name_opt
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(catalog_id.as_str());
    let cdesc = description_opt
        .map(str::trim)
        .unwrap_or("Local imported DuckLake catalog");
    let corg = {
        let o = organism_raw.trim();
        if o.is_empty() { "Homo sapiens" } else { o }
    };
    let commit_msg = commit_message_opt
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "CLI import '{}' ({}, {}) into table '{}'",
                clean_fname,
                format.as_str(),
                mode.as_str(),
                table_name
            )
        });

    let result = (|| -> CompassResult<_> {
        let report = validate_and_normalize_quarantine_dir(&in_dir, &out_dir)?;
        let norm_pq = out_dir.join("normalized.parquet");
        let eng = DuckDbCatalogEngine::create_or_import_local_parquet(
            &cat_dir,
            catalog_id.clone(),
            cname,
            cdesc,
            corg,
            table_name,
            mode,
            &report.primary_key,
            &norm_pq,
            &commit_msg,
        )?;
        Ok((report, eng.manifest().clone()))
    })();
    let _ = std::fs::remove_dir_all(&stage_root);
    let (report, summary) = result?;

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "plane": "local",
            "catalog_dir": cat_dir,
            "report": report,
            "catalog": summary,
        }))
        .unwrap_or_default()
    );
    Ok(ExitCode::SUCCESS)
}
