//! `biocompass` single-binary CLI (`up`, `init-plugin`, `run`, `verify-receipt`).

use biocompass::server::build_router;
use biocompass::state::{AppState, DeploymentMode};
use biocompass_core_traits::{CompassError, CompassResult, PluginId, PrepareRunRequest, RunStatus};
use biocompass_plugin_spec::{LoadedPlugin, TemplateKind, write_template_to_dir};
use clap::{Parser, Subcommand};
use froglet_verify::{extract_documents, validate_chain_documents};
use serde_json::{Value, json};
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
    /// Execute a two-step `Prepare -> Confirm` container run from the CLI and emit a signed Froglet chain.
    Run {
        /// Path to a plugin directory containing `plugin.toml`, or a registered `<org>/<slug>` ID.
        #[arg(long)]
        plugin: String,
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
        Commands::Up {
            host,
            port,
            data_dir,
            mode,
            allow_local_dev_tags,
        } => {
            let deploy_mode = DeploymentMode::parse(&mode)?;
            let dir = data_dir.unwrap_or_else(default_data_dir);
            let state = AppState::bootstrap(&dir, deploy_mode, allow_local_dev_tags).await?;
            let addr: SocketAddr = format!("{host}:{port}")
                .parse()
                .map_err(|e| CompassError::Validation(format!("invalid listen address: {e}")))?;

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

            let router = build_router(state);
            axum::serve(listener, router)
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
        Commands::Run {
            plugin,
            params,
            grant,
            data_dir,
            allow_local_dev_tags,
            receipt_out,
        } => {
            let dir = data_dir.unwrap_or_else(default_data_dir);
            let state = AppState::bootstrap(&dir, DeploymentMode::Local, allow_local_dev_tags).await?;
            let params_val: Value = serde_json::from_str(&params)
                .map_err(|e| CompassError::Validation(format!("invalid --params JSON: {e}")))?;

            let plugin_path = Path::new(&plugin);
            let plugin_id = if plugin_path.is_dir() && plugin_path.join("plugin.toml").is_file() {
                let loaded = LoadedPlugin::load_from_dir(plugin_path, allow_local_dev_tags)?;
                let pid = loaded.manifest.id.clone();
                state.registry.register_plugin(loaded).await?;
                pid
            } else {
                PluginId::new(plugin).map_err(CompassError::Validation)?
            };

            let dataset_grant = state.resolve_grant(Some(&grant)).await?;
            let quote = state
                .dispatcher
                .prepare_run(PrepareRunRequest {
                    org_id: state.default_org_id.clone(),
                    session_id: None,
                    plugin_id,
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
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
