//! Host-process mailbox responder used by the notebook module's end-to-end test (`N-5`).
//!
//! Runs the real [`supervise`] loop and [`biocompass_oci_runner::FileMailbox`] against a host
//! subprocess instead of a container, so the Python client can be exercised against the Rust
//! mailbox on a laptop without Docker. The child broker is a fixed fake:
//!
//! - `alithea/echo`: ok, `result_json = {"echo": params}`, receipt `"ab" * 32`, artifact `echo.txt`.
//! - `alithea/big`: ok, `result_json` larger than the 1 MiB reply cap (delivered via `result_path`).
//! - `alithea/fail`: error reply `CHILD_RUN_FAILED`.
//!
//! Usage:
//! `mailbox_process_responder --in-dir D --out-dir D --child-modules caller|a,b
//!  [--max-child-wait-sec N] [--max-runtime-sec N] -- <program> [args...]`
//!
//! The subprocess inherits the environment plus `BIOCOMPASS_IN_DIR` / `BIOCOMPASS_OUT_DIR`.
//! The last stdout line is `{"exit_code", "child_receipt_hashes", "error"}`.

use biocompass_core_traits::{CompassError, CompassResult, OciImageRef, RunId, RunStatus};
use biocompass_oci_runner::{
    ChildModuleRequest, ChildRunBroker, ChildRunBrokerContext, ChildRunReply,
    ChildRunStagedArtifact, ContainerHandle, ContainerRunSpec, ContainerRunner, EngineKind,
    SuperviseOptions, child_artifact_alias, prepare_stage_permissions, stage_child_artifact_bytes,
    supervise,
};
use biocompass_plugin_spec::{ChildModules, NetworkPolicy, PluginResources};
use serde_json::json;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncRead;
use tokio::process::{Child, Command};

struct ProcessHandle {
    name: String,
    child: Child,
}

#[async_trait::async_trait]
impl ContainerHandle for ProcessHandle {
    fn container_name(&self) -> &str {
        &self.name
    }
    fn take_stdout(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>> {
        self.child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn AsyncRead + Send + Unpin>)
    }
    fn take_stderr(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>> {
        self.child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn AsyncRead + Send + Unpin>)
    }
    async fn wait(&mut self) -> CompassResult<i32> {
        let status = self
            .child
            .wait()
            .await
            .map_err(|e| CompassError::ContainerEngine(format!("wait: {e}")))?;
        Ok(status.code().unwrap_or(137))
    }
    async fn kill(&mut self) -> CompassResult<()> {
        let _ = self.child.kill().await;
        Ok(())
    }
}

struct ProcessRunner {
    program: String,
    args: Vec<String>,
}

#[async_trait::async_trait]
impl ContainerRunner for ProcessRunner {
    fn engine_kind(&self) -> EngineKind {
        EngineKind::Docker
    }
    fn engine_version(&self) -> String {
        "host-process".into()
    }
    async fn spawn(&self, spec: &ContainerRunSpec) -> CompassResult<Box<dyn ContainerHandle>> {
        let child = Command::new(&self.program)
            .args(&self.args)
            .env("BIOCOMPASS_IN_DIR", &spec.in_dir)
            .env("BIOCOMPASS_OUT_DIR", &spec.out_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| CompassError::ContainerEngine(format!("spawn {}: {e}", self.program)))?;
        Ok(Box::new(ProcessHandle {
            name: format!("proc-{}", spec.run_id.as_str()),
            child,
        }))
    }
    async fn cancel(&self, _run_id: &RunId) -> CompassResult<()> {
        Ok(())
    }
}

struct FakeBroker;

#[async_trait::async_trait]
impl ChildRunBroker for FakeBroker {
    async fn run_child(
        &self,
        ctx: &ChildRunBrokerContext,
        request: ChildModuleRequest,
    ) -> CompassResult<ChildRunReply> {
        let mut reply = ChildRunReply {
            request_id: request.request_id.clone(),
            ok: true,
            status: Some(RunStatus::Succeeded),
            run_id: Some(format!("run-child-{}", request.request_id)),
            receipt_hash: Some("ab".repeat(32)),
            result_json: None,
            result_path: None,
            artifacts: Vec::new(),
            error_code: None,
            error: None,
        };
        match request.plugin_id.as_str() {
            "alithea/echo" => {
                let path = stage_child_artifact_bytes(
                    &ctx.parent_in_dir,
                    &child_artifact_alias(&request.request_id),
                    "echo.txt",
                    b"echo",
                )?;
                reply.result_json = Some(json!({ "echo": request.params }));
                reply.artifacts.push(ChildRunStagedArtifact {
                    name: "echo.txt".into(),
                    sha256: "092c79e8f80e559e404bcf660c48f3522b67aba9ff1484b0367e1a4ddef7431d".into(),
                    size_bytes: 4,
                    media_type: "text/plain".into(),
                    staged_path: Some(path),
                });
            }
            "alithea/big" => {
                reply.receipt_hash = Some("cd".repeat(32));
                reply.result_json = Some(json!({ "blob": "x".repeat(1_200_000) }));
            }
            other => {
                return Ok(ChildRunReply::error(
                    request.request_id,
                    "CHILD_RUN_FAILED",
                    format!("fake broker has no module '{other}'"),
                ));
            }
        }
        Ok(reply)
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: mailbox_process_responder --in-dir D --out-dir D --child-modules caller|a,b \
         [--max-child-wait-sec N] [--max-runtime-sec N] -- <program> [args...]"
    );
    std::process::exit(2);
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let (mut in_dir, mut out_dir, mut child_modules) = (None, None, None);
    let (mut max_wait, mut max_runtime) = (30_u64, 60_u64);
    let mut command = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--in-dir" => in_dir = args.next().map(PathBuf::from),
            "--out-dir" => out_dir = args.next().map(PathBuf::from),
            "--child-modules" => child_modules = args.next(),
            "--max-child-wait-sec" => {
                max_wait = args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage())
            }
            "--max-runtime-sec" => {
                max_runtime = args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage())
            }
            "--" => {
                command.extend(args.by_ref());
            }
            _ => usage(),
        }
    }
    let (Some(in_dir), Some(out_dir), Some(child_modules)) = (in_dir, out_dir, child_modules)
    else {
        usage()
    };
    if command.is_empty() {
        usage();
    }
    let child_modules = if child_modules == "caller" {
        ChildModules::Caller
    } else {
        ChildModules::List(
            child_modules
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
        )
    };
    let root = in_dir
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let outcome = async {
        prepare_stage_permissions(&root, &in_dir, &out_dir)?;
        let spec = ContainerRunSpec {
            run_id: RunId::from("run-mailbox-e2e"),
            image: OciImageRef::parse("local/process:dev", true)?,
            in_dir: in_dir.clone(),
            out_dir: out_dir.clone(),
            resources: PluginResources {
                cpus: 1,
                memory_mb: 256,
                gpu: 0,
                timeout_sec: max_runtime,
                pids_limit: 64,
            },
            network: NetworkPolicy::default(),
            command_override: None,
        };
        let runner = ProcessRunner {
            program: command[0].clone(),
            args: command[1..].to_vec(),
        };
        supervise(
            &runner,
            &spec,
            SuperviseOptions {
                child_broker: Some(Arc::new(FakeBroker)),
                allowed_child_modules: child_modules,
                max_child_wait_sec: Some(max_wait),
                ..SuperviseOptions::default()
            },
        )
        .await
    }
    .await;
    match outcome {
        Ok(report) => {
            for line in report.stdout_lines.iter().chain(report.stderr_lines.iter()) {
                eprintln!("[module] {line}");
            }
            println!(
                "{}",
                json!({
                    "exit_code": report.exit_code,
                    "child_receipt_hashes": report.child_receipt_hashes,
                    "error": null,
                })
            );
        }
        Err(err) => {
            println!(
                "{}",
                json!({ "exit_code": null, "child_receipt_hashes": [], "error": err.to_string() })
            );
            std::process::exit(1);
        }
    }
}
