//! `biocompass-oci-runner`: Hardened rootless Podman / VM-isolated Docker OCI container runner
//! enforcing `/in:ro`, `/out:rw`, `--read-only`, `--user 65532:65532`, `--cap-drop=ALL`,
//! `--security-opt=no-new-privileges`, `--network=none`, `--pids-limit`, and hard deadlines.

pub mod registry;

use biocompass_core_traits::{CompassError, CompassResult, OciImageRef, RunId, RunLogEvent};
use biocompass_plugin_spec::{NetworkPolicy, PluginResources};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::broadcast;
use uuid::Uuid;

pub use registry::OciDistributionClient;

/// Detected host OCI runtime CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedEngine {
    pub kind: EngineKind,
    pub binary_path: PathBuf,
    pub server_version: String,
    pub rootless: bool,
}

/// Supported OCI runtime backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    Podman,
    Docker,
}

impl EngineKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Podman => "podman",
            Self::Docker => "docker",
        }
    }
}

/// Specification for a single hardened container execution.
#[derive(Debug, Clone)]
pub struct ContainerRunSpec {
    pub run_id: RunId,
    pub image: OciImageRef,
    pub in_dir: PathBuf,
    pub out_dir: PathBuf,
    pub resources: PluginResources,
    pub network: NetworkPolicy,
    /// Optional entrypoint + args override used by security/confinement verification probes.
    pub command_override: Option<(String, Vec<String>)>,
}

/// Completed container execution telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerExecutionReport {
    pub engine: EngineKind,
    pub engine_version: String,
    pub container_name: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub stdout_lines: Vec<String>,
    pub stderr_lines: Vec<String>,
}

/// Probe candidate container engines (`podman` preferred on Linux; `podman` or `docker` on macOS/Windows).
pub async fn detect_container_engine() -> CompassResult<DetectedEngine> {
    let podman_candidates = ["/opt/podman/bin/podman", "/usr/bin/podman", "podman"];
    let docker_candidates = [
        "/usr/local/bin/docker",
        "/opt/homebrew/bin/docker",
        "/usr/bin/docker",
        "docker",
    ];

    if cfg!(target_os = "linux") {
        for bin in podman_candidates {
            if let Some(eng) = probe_podman(bin).await {
                return Ok(eng);
            }
        }
        for bin in docker_candidates {
            if let Some(eng) = probe_docker(bin).await {
                return Ok(eng);
            }
        }
    } else {
        for bin in docker_candidates {
            if let Some(eng) = probe_docker(bin).await {
                return Ok(eng);
            }
        }
        for bin in podman_candidates {
            if let Some(eng) = probe_podman(bin).await {
                return Ok(eng);
            }
        }
    }

    Err(CompassError::ContainerEngine(
        "no active Podman or Docker engine reachable; start rootless Podman or Docker Desktop".into(),
    ))
}

async fn probe_podman(bin: &str) -> Option<DetectedEngine> {
    let out = tokio::time::timeout(
        Duration::from_secs(4),
        Command::new(bin)
            .args([
                "info",
                "--format",
                "{{.Version.Version}}|{{.Host.Security.Rootless}}",
            ])
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let (ver, rootless_str) = text.split_once('|').unwrap_or((&text, "false"));
    Some(DetectedEngine {
        kind: EngineKind::Podman,
        binary_path: PathBuf::from(bin),
        server_version: ver.trim().to_owned(),
        rootless: rootless_str.trim() == "true",
    })
}

async fn probe_docker(bin: &str) -> Option<DetectedEngine> {
    let out = tokio::time::timeout(
        Duration::from_secs(4),
        Command::new(bin)
            .args(["info", "--format", "{{.ServerVersion}}|{{.SecurityOptions}}"])
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let (ver, sec_opts) = text.split_once('|').unwrap_or((&text, ""));
    if ver.trim().is_empty() {
        return None;
    }
    Some(DetectedEngine {
        kind: EngineKind::Docker,
        binary_path: PathBuf::from(bin),
        server_version: ver.trim().to_owned(),
        rootless: sec_opts.contains("rootless"),
    })
}

/// Construct hardened `podman run` / `docker run` CLI arguments enforcing all security invariants.
pub fn build_hardened_oci_args(
    engine: &DetectedEngine,
    container_name: &str,
    spec: &ContainerRunSpec,
) -> CompassResult<Vec<String>> {
    if !spec.in_dir.is_absolute() || !spec.out_dir.is_absolute() {
        return Err(CompassError::Validation(
            "container /in and /out host stage paths must be absolute".into(),
        ));
    }

    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--name".to_string(),
        container_name.to_string(),
        "--user".to_string(),
        "65532:65532".to_string(),
        "--read-only".to_string(),
        "--cap-drop=ALL".to_string(),
        "--security-opt".to_string(),
        "no-new-privileges".to_string(),
        "--pids-limit".to_string(),
        spec.resources.pids_limit.to_string(),
        "--cpus".to_string(),
        spec.resources.cpus.to_string(),
        "--memory".to_string(),
        format!("{}m", spec.resources.memory_mb),
        "--tmpfs".to_string(),
        "/tmp:rw,noexec,nosuid,nodev,size=512m".to_string(),
    ];

    if cfg!(target_os = "linux") && engine.kind == EngineKind::Podman && engine.rootless {
        args.push("--security-opt".into());
        args.push("label=disable".into());
    }

    if spec.network.is_airgapped() {
        args.push("--network=none".into());
    } else {
        // Explicit domain allowlists require egress proxy network; default bridge only when declared.
        args.push("--network=bridge".into());
    }

    if spec.resources.gpu > 0 {
        match engine.kind {
            EngineKind::Podman => {
                args.push("--device".into());
                args.push("nvidia.com/gpu=all".into());
            }
            EngineKind::Docker => {
                args.push("--gpus".into());
                args.push(spec.resources.gpu.to_string());
            }
        }
    }

    args.push("-v".into());
    args.push(format!("{}:/in:ro", spec.in_dir.display()));
    args.push("-v".into());
    args.push(format!("{}:/out:rw", spec.out_dir.display()));

    if let Some((entrypoint, _)) = &spec.command_override {
        args.push("--entrypoint".into());
        args.push(entrypoint.clone());
    }

    args.push(spec.image.raw.clone());

    if let Some((_, cmd_args)) = &spec.command_override {
        args.extend(cmd_args.iter().cloned());
    }

    Ok(args)
}

/// Prepare `/in` (`0755`) and `/out` (`0777` inside a `0700` parent workspace) so UID `65532:65532`
/// can read `/in` and write `/out` across both Linux rootless Podman and macOS Docker VirtioFS.
pub fn prepare_stage_permissions(stage_root: &Path, in_dir: &Path, out_dir: &Path) -> CompassResult<()> {
    std::fs::create_dir_all(in_dir)
        .map_err(|e| CompassError::Storage(format!("create /in stage dir: {e}")))?;
    std::fs::create_dir_all(out_dir)
        .map_err(|e| CompassError::Storage(format!("create /out stage dir: {e}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(stage_root, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| CompassError::Storage(format!("chmod 0700 stage_root: {e}")))?;
        std::fs::set_permissions(in_dir, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| CompassError::Storage(format!("chmod 0755 in_dir: {e}")))?;
        std::fs::set_permissions(out_dir, std::fs::Permissions::from_mode(0o777))
            .map_err(|e| CompassError::Storage(format!("chmod 0777 out_dir: {e}")))?;
    }
    Ok(())
}

/// Hardened OCI container runner executing real Podman/Docker containers with live log streaming.
#[derive(Debug, Clone)]
pub struct OciContainerRunner {
    engine: DetectedEngine,
}

impl OciContainerRunner {
    /// Detect and bind to an active host Podman/Docker engine.
    pub async fn detect() -> CompassResult<Self> {
        let engine = detect_container_engine().await?;
        Ok(Self { engine })
    }

    /// Construct from a known `DetectedEngine`.
    pub const fn from_engine(engine: DetectedEngine) -> Self {
        Self { engine }
    }

    /// Inspect the bound container engine metadata.
    pub const fn engine(&self) -> &DetectedEngine {
        &self.engine
    }

    /// Execute a hardened OCI container, streaming `stdout`/`stderr` lines and enforcing `timeout_sec`.
    pub async fn execute(
        &self,
        spec: &ContainerRunSpec,
        log_tx: Option<broadcast::Sender<RunLogEvent>>,
    ) -> CompassResult<ContainerExecutionReport> {
        let container_name = format!("biocompass-job-{}", Uuid::new_v4().simple());
        let args = build_hardened_oci_args(&self.engine, &container_name, spec)?;

        let started = Instant::now();
        let mut child = Command::new(&self.engine.binary_path)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                CompassError::ContainerEngine(format!(
                    "spawn {} failed: {e}",
                    self.engine.binary_path.display()
                ))
            })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CompassError::ContainerEngine("capture container stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| CompassError::ContainerEngine("capture container stderr".into()))?;

        let run_id_out = spec.run_id.clone();
        let tx_out = log_tx.clone();
        let stdout_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            let mut lines = Vec::new();
            while let Ok(Some(line)) = reader.next_line().await {
                if let Some(tx) = &tx_out {
                    let _ = tx.send(RunLogEvent {
                        run_id: run_id_out.clone(),
                        timestamp: Utc::now(),
                        stream: "stdout".into(),
                        line: line.clone(),
                    });
                }
                if lines.len() < 2_000 {
                    lines.push(line);
                }
            }
            lines
        });

        let run_id_err = spec.run_id.clone();
        let tx_err = log_tx;
        let stderr_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            let mut lines = Vec::new();
            while let Ok(Some(line)) = reader.next_line().await {
                if let Some(tx) = &tx_err {
                    let _ = tx.send(RunLogEvent {
                        run_id: run_id_err.clone(),
                        timestamp: Utc::now(),
                        stream: "stderr".into(),
                        line: line.clone(),
                    });
                }
                if lines.len() < 2_000 {
                    lines.push(line);
                }
            }
            lines
        });

        let timeout_dur = Duration::from_secs(spec.resources.timeout_sec.max(1));
        let wait_res = tokio::time::timeout(timeout_dur, child.wait()).await;

        match wait_res {
            Ok(Ok(status)) => {
                let stdout_lines = stdout_task.await.unwrap_or_default();
                let stderr_lines = stderr_task.await.unwrap_or_default();
                let exit_code = status.code().unwrap_or(-1);
                Ok(ContainerExecutionReport {
                    engine: self.engine.kind,
                    engine_version: self.engine.server_version.clone(),
                    container_name,
                    exit_code,
                    duration_ms: started.elapsed().as_millis() as u64,
                    stdout_lines,
                    stderr_lines,
                })
            }
            Ok(Err(e)) => {
                let _ = self.force_remove_container(&container_name).await;
                Err(CompassError::ContainerEngine(format!(
                    "wait on container {container_name} failed: {e}"
                )))
            }
            Err(_) => {
                let _ = child.kill().await;
                let _ = self.force_remove_container(&container_name).await;
                Err(CompassError::ContainerEngine(format!(
                    "container {container_name} exceeded hard timeout of {}s and was terminated",
                    spec.resources.timeout_sec
                )))
            }
        }
    }

    async fn force_remove_container(&self, container_name: &str) -> CompassResult<()> {
        let _ = Command::new(&self.engine.binary_path)
            .args(["rm", "-f", container_name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardened_oci_args_enforce_nonroot_readonly_capdrop_no_network_and_ro_in_mount() {
        let engine = DetectedEngine {
            kind: EngineKind::Podman,
            binary_path: PathBuf::from("/usr/bin/podman"),
            server_version: "5.5.1".into(),
            rootless: true,
        };
        let image = OciImageRef::parse(
            "127.0.0.1:5500/alithea/hla-motif-enrichment@sha256:8f3a9d0b1c2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f80",
            false,
        )
        .expect("image");
        let spec = ContainerRunSpec {
            run_id: RunId::from("run-sec-test"),
            image,
            in_dir: PathBuf::from("/tmp/stage/in"),
            out_dir: PathBuf::from("/tmp/stage/out"),
            resources: PluginResources {
                cpus: 4,
                memory_mb: 8192,
                gpu: 0,
                timeout_sec: 120,
                pids_limit: 256,
            },
            network: NetworkPolicy::default(),
            command_override: None,
        };

        let args = build_hardened_oci_args(&engine, "biocompass-unit", &spec).expect("args");
        assert!(args.contains(&"--read-only".to_string()));
        assert!(args.contains(&"--cap-drop=ALL".to_string()));
        assert!(args.contains(&"no-new-privileges".to_string()));
        assert!(args.contains(&"--network=none".to_string()));
        assert!(args.contains(&"65532:65532".to_string()));
        assert!(args.contains(&"/tmp:rw,noexec,nosuid,nodev,size=512m".to_string()));
        assert!(args.contains(&"/tmp/stage/in:/in:ro".to_string()));
        assert!(args.contains(&"/tmp/stage/out:/out:rw".to_string()));
    }
}
