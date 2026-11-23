//! `biocompass-oci-runner`: Hardened rootless Podman / VM-isolated Docker OCI container runner
//! enforcing `/in:ro`, `/out:rw`, `--read-only`, `--user 65532:65532`, `--cap-drop=ALL`,
//! `--security-opt=no-new-privileges`, `--network=none`, `--pids-limit`, and hard deadlines.

pub mod registry;
mod cleanup;
pub use cleanup::AbandonedContainer;
mod release_archive;
mod worker_cache;
#[cfg(all(test, unix))]
mod image_preparation_tests;
#[cfg(all(test, unix))]
mod cleanup_tests;

use biocompass_core_traits::{CompassError, CompassResult, OciImageRef, RunId, RunLogEvent};
use biocompass_core_traits::types::OCI_IMAGE_PREPARATION_TIMEOUT_SECS;
use biocompass_plugin_spec::{NetworkPolicy, PluginResources};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::broadcast;
use uuid::Uuid;

pub use registry::OciDistributionClient;

/// Start of a live query request on stdout and its response on stdin.
pub const LIVE_QUERY_PREFIX: &[u8] = b"BIOCOMPASS_QUERY_V1 ";
const LIVE_QUERY_MARKER: &[u8] = b"BIOCOMPASS_QUERY_V1";
const MAX_QUERY_REQUEST_BYTES: usize = 8_192;
const MAX_QUERY_RESPONSE_BYTES: usize = 1_048_576;

/// Handles a frame for an already-authorized, air-gapped run.
#[async_trait::async_trait]
pub trait LiveQueryResponder: Send + Sync {
    async fn call(&self, frame: &[u8]) -> CompassResult<Vec<u8>>;
}

struct LiveQueryChannel<W> {
    stdin: W,
    responder: Arc<dyn LiveQueryResponder>,
}

impl<W: AsyncWrite + Unpin> LiveQueryChannel<W> {
    async fn reply(&mut self, line: &[u8]) -> CompassResult<()> {
        let frame = line
            .strip_prefix(LIVE_QUERY_PREFIX)
            .filter(|frame| !frame.is_empty() && !frame.contains(&b'\r'))
            .ok_or_else(|| {
                CompassError::ContainerEngine("malformed live query request frame".into())
            })?;
        let response = self.responder.call(frame).await?;
        if response.is_empty()
            || response.len() > MAX_QUERY_RESPONSE_BYTES
            || response.iter().any(|byte| matches!(*byte, b'\n' | b'\r'))
        {
            return Err(CompassError::ContainerEngine(
                "invalid live query response frame".into(),
            ));
        }
        let write = async {
            self.stdin.write_all(LIVE_QUERY_PREFIX).await?;
            self.stdin.write_all(&response).await?;
            self.stdin.write_all(b"\n").await
        };
        write
            .await
            .map_err(|e| CompassError::ContainerEngine(format!("write live query response: {e}")))
    }
}

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
    #[serde(default)]
    pub log_bytes_total: u64,
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
    detect_engine_with_candidates(cfg!(target_os = "linux"), &podman_candidates, &docker_candidates)
        .await
}

async fn detect_engine_with_candidates(
    is_linux: bool,
    podman_candidates: &[&str],
    docker_candidates: &[&str],
) -> CompassResult<DetectedEngine> {
    if is_linux {
        for bin in podman_candidates {
            if let Some(eng) = probe_podman(bin).await {
                if !eng.rootless {
                    tracing::warn!(
                        binary = %eng.binary_path.display(),
                        version = %eng.server_version,
                        "Podman on Linux is running in rootful mode; rootless Podman is recommended for stronger host user-namespace isolation"
                    );
                }
                return Ok(eng);
            }
        }
        for bin in docker_candidates {
            if let Some(eng) = probe_docker(bin).await {
                if !eng.rootless {
                    tracing::warn!(
                        binary = %eng.binary_path.display(),
                        version = %eng.server_version,
                        "Rootless Podman unavailable on Linux; falling back to standard Docker engine (rootless=false) — rootless Podman is recommended for stronger host user-namespace isolation"
                    );
                }
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
/// By default (cloud / marketplace / edge workers), non-airgapped network policies are rejected (`SEC-12`).
pub fn build_hardened_oci_args(
    engine: &DetectedEngine,
    container_name: &str,
    spec: &ContainerRunSpec,
) -> CompassResult<Vec<String>> {
    build_hardened_oci_args_with_local_allowance(engine, container_name, spec, false)
}

fn build_hardened_oci_args_with_local_allowance(
    engine: &DetectedEngine,
    container_name: &str,
    spec: &ContainerRunSpec,
    allow_local_bridge_egress: bool,
) -> CompassResult<Vec<String>> {
    if !spec.in_dir.is_absolute() || !spec.out_dir.is_absolute() {
        return Err(CompassError::Validation(
            "container /in and /out host stage paths must be absolute".into(),
        ));
    }

    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--log-driver=none".to_string(),
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
        "--memory-swap".to_string(),
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
    } else if allow_local_bridge_egress
        && let NetworkPolicy::Allowlist { allow_domains } = &spec.network
        && !allow_domains.is_empty()
    {
        tracing::warn!(
            run_id = %spec.run_id,
            ?allow_domains,
            "Local mode allow_domains enabled: running container with --network=bridge (domain-level egress filtering is not enforced locally)"
        );
        args.push("--network=bridge".into());
    } else {
        return Err(CompassError::AccessDenied(
            "non-airgapped container network policy is rejected outside local mode (--network=none required)".into(),
        ));
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

fn hash_bundled_build_context(root: &Path) -> CompassResult<String> {
    use sha2::{Digest, Sha256};

    fn walk(base: &Path, dir: &Path, entries: &mut Vec<(String, PathBuf)>) -> CompassResult<()> {
        for item in std::fs::read_dir(dir)
            .map_err(|e| CompassError::Storage(format!("read {}: {e}", dir.display())))?
        {
            let item = item.map_err(|e| CompassError::Storage(e.to_string()))?;
            let path = item.path();
            let rel = path
                .strip_prefix(base)
                .map_err(|e| CompassError::Internal(e.to_string()))?
                .to_string_lossy()
                .replace('\\', "/");
            if rel == "ui" || rel.starts_with("ui/") || rel.contains("__pycache__") {
                continue;
            }
            let ft = item
                .file_type()
                .map_err(|e| CompassError::Storage(e.to_string()))?;
            if ft.is_dir() {
                walk(base, &path, entries)?;
            } else if ft.is_file() {
                entries.push((rel, path));
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (rel, path) in files {
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        let bytes = std::fs::read(&path)
            .map_err(|e| CompassError::Storage(format!("read {}: {e}", path.display())))?;
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(&bytes);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Hardened OCI container runner executing real Podman/Docker containers with live log streaming.
#[derive(Debug, Clone)]
pub struct OciContainerRunner {
    engine: DetectedEngine,
    worker_cache: Option<Arc<worker_cache::WorkerCache>>,
    workspace_scope: Option<String>,
}

impl OciContainerRunner {
    /// Detect and bind to an active host Podman/Docker engine.
    pub async fn detect() -> CompassResult<Self> {
        let engine = detect_container_engine().await?;
        Ok(Self::from_engine(engine))
    }

    /// Construct from a known `DetectedEngine`.
    pub const fn from_engine(engine: DetectedEngine) -> Self {
        Self { engine, worker_cache: None, workspace_scope: None }
    }

    /// Inspect the bound container engine metadata.
    pub const fn engine(&self) -> &DetectedEngine {
        &self.engine
    }

    /// Return `true` when the given OCI image reference already exists in the local engine store.
    pub async fn has_local_image(&self, image_ref: &str) -> bool {
        let args: Vec<&str> = match self.engine.kind {
            EngineKind::Docker => vec!["image", "inspect", "--format", "{{.Id}}", image_ref],
            EngineKind::Podman => vec!["image", "exists", image_ref],
        };
        let res = tokio::time::timeout(
            Duration::from_secs(4),
            Command::new(&self.engine.binary_path)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
        )
        .await;
        matches!(res, Ok(Ok(status)) if status.success())
    }

    /// Resolve a locally installed image to the exact content the native run will execute.
    /// Dev tags are allowed only under the existing local-development configuration.
    pub async fn installed_execution_image(&self, image: &str, allow_dev: bool) -> CompassResult<OciImageRef> {
        let mut pinned = OciImageRef::parse(image, allow_dev)?;
        if pinned.sha256_digest.is_some() {
            if self.has_verified_local_digest(image).await {
                return Ok(pinned);
            }
        } else {
            let output = tokio::time::timeout(Duration::from_secs(4),
                Command::new(&self.engine.binary_path)
                    .args(["image","inspect","--format","{{.Id}}",image])
                    .stdin(Stdio::null()).kill_on_drop(true).output()
            ).await.map_err(|_| CompassError::ContainerEngine("local image inspection timed out".into()))?
             .map_err(|e| CompassError::ContainerEngine(e.to_string()))?;
            let id = String::from_utf8_lossy(&output.stdout);
            let digest = id.trim().strip_prefix("sha256:").unwrap_or(id.trim());
            if output.status.success() && digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                pinned.raw = format!("sha256:{digest}");
                return Ok(pinned);
            }
        }
        Err(CompassError::NotFound(format!("image is not installed locally: {image}")))
    }

    async fn inspect_local_image_id(&self, image_ref: &str) -> CompassResult<Option<String>> {
        let output = tokio::time::timeout(
            Duration::from_secs(4),
            Command::new(&self.engine.binary_path)
                .args(["image", "inspect", "--format", "{{.Id}}", image_ref])
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| CompassError::ContainerEngine("local image inspection timed out".into()))?
        .map_err(|e| CompassError::ContainerEngine(e.to_string()))?;
        if !output.status.success() {
            return Ok(None);
        }
        let id = String::from_utf8_lossy(&output.stdout);
        let digest = id.trim().strip_prefix("sha256:").unwrap_or(id.trim());
        if digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Ok(Some(digest.to_owned()))
        } else {
            Ok(None)
        }
    }

    /// Deterministic local stage tag for a bundled plugin `Dockerfile` context (`R11`).
    pub fn bundled_local_stage_tag(
        context_dir: &Path,
        slug: &str,
        version: &str,
    ) -> CompassResult<(String, String)> {
        let dockerfile = context_dir.join("Dockerfile");
        if !dockerfile.is_file() {
            return Err(CompassError::NotFound(format!(
                "bundled Dockerfile missing at {}",
                dockerfile.display()
            )));
        }
        let safe_slug: String = slug
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        let safe_ver: String = version
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let content_hash = hash_bundled_build_context(context_dir)?;
        let stage_tag = format!("local/bundled-{safe_slug}:stage-{safe_ver}-{}", &content_hash[..12]);
        Ok((safe_slug, stage_tag))
    }

    /// Return whether the local clean-clone fallback build for a bundled plugin already exists (`R11`).
    pub async fn has_local_bundled_image(
        &self,
        context_dir: &Path,
        slug: &str,
        version: &str,
    ) -> bool {
        match Self::bundled_local_stage_tag(context_dir, slug, version) {
            Ok((_, stage_tag)) => self.has_local_image(&stage_tag).await,
            Err(_) => false,
        }
    }

    /// Build (or reuse cached) a bundled local plugin `Dockerfile` under `local/bundled-<slug>:sha256-<id>`
    /// when the pinned registry is unreachable on a clean local clone (R11).
    pub async fn build_local_bundled_image(
        &self,
        context_dir: &Path,
        slug: &str,
        version: &str,
    ) -> CompassResult<OciImageRef> {
        let dockerfile = context_dir.join("Dockerfile");
        let (safe_slug, stage_tag) = Self::bundled_local_stage_tag(context_dir, slug, version)?;

        let digest = match self.inspect_local_image_id(&stage_tag).await? {
            Some(existing) => existing,
            None => {
                let out = tokio::time::timeout(
                    Duration::from_secs(OCI_IMAGE_PREPARATION_TIMEOUT_SECS),
                    Command::new(&self.engine.binary_path)
                        .args(["build", "-t", &stage_tag])
                        .arg(context_dir)
                        .kill_on_drop(true)
                        .stdin(Stdio::null())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .output(),
                )
                .await
                .map_err(|_| {
                    CompassError::ContainerEngine(format!(
                        "timed out building bundled Dockerfile '{}'",
                        dockerfile.display()
                    ))
                })?
                .map_err(|e| {
                    CompassError::ContainerEngine(format!(
                        "spawn {} build failed: {e}",
                        self.engine.binary_path.display()
                    ))
                })?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
                    return Err(CompassError::ContainerEngine(format!(
                        "failed building bundled Dockerfile '{}': {}",
                        dockerfile.display(),
                        if stderr.is_empty() {
                            format!("exit code {:?}", out.status.code())
                        } else {
                            stderr
                        }
                    )));
                }
                self.inspect_local_image_id(&stage_tag)
                    .await?
                    .ok_or_else(|| {
                        CompassError::ContainerEngine(format!(
                            "built bundled image '{stage_tag}' could not be inspected"
                        ))
                    })?
            }
        };

        let pinned_local_tag = format!("local/bundled-{safe_slug}:sha256-{digest}");
        if self.inspect_local_image_id(&pinned_local_tag).await?.as_deref() != Some(digest.as_str())
        {
            let tag_out = tokio::time::timeout(
                Duration::from_secs(10),
                Command::new(&self.engine.binary_path)
                    .args(["tag", &stage_tag, &pinned_local_tag])
                    .kill_on_drop(true)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .output(),
            )
            .await
            .map_err(|_| CompassError::ContainerEngine("local image tag timed out".into()))?
            .map_err(|e| CompassError::ContainerEngine(format!("tag bundled image failed: {e}")))?;
            if !tag_out.status.success() {
                let stderr = String::from_utf8_lossy(&tag_out.stderr).trim().to_owned();
                return Err(CompassError::ContainerEngine(format!(
                    "failed tagging bundled image '{pinned_local_tag}': {stderr}"
                )));
            }
        }
        OciImageRef::parse(&pinned_local_tag, true)
    }

    /// Verify at `confirm` time that a `local/bundled-<slug>:sha256-<digest>` quote still resolves
    /// to the exact local container image ID recorded when the quote was signed.
    pub async fn verify_local_bundled_image(&self, image_ref: &str) -> CompassResult<OciImageRef> {
        let parsed = OciImageRef::parse(image_ref, true)?;
        let Some(expected_digest) = parsed
            .local_tag
            .as_deref()
            .filter(|_| parsed.repository.starts_with("local/bundled-"))
            .and_then(|t| t.strip_prefix("sha256-"))
        else {
            return Err(CompassError::Validation(format!(
                "invalid local bundled image reference '{image_ref}'"
            )));
        };
        let actual = self.inspect_local_image_id(&parsed.raw).await?.ok_or_else(|| {
            CompassError::NotFound(format!(
                "locally built bundled image '{image_ref}' is no longer present"
            ))
        })?;
        if actual != expected_digest {
            return Err(CompassError::IntegrityViolation(format!(
                "local bundled image '{image_ref}' ID changed (expected {expected_digest}, got {actual})"
            )));
        }
        Ok(parsed)
    }

    /// Pull an OCI image into the bound container engine store.
    pub async fn pull_image(&self, image_ref: &str) -> CompassResult<()> {
        if self.worker_cache.is_some() {
            return self.prepare_for_execution(image_ref).await.map(|_| ());
        }
        self.pull_uncached(image_ref).await
    }

    async fn pull_uncached(&self, image_ref: &str) -> CompassResult<()> {
        let out = tokio::time::timeout(
            Duration::from_secs(OCI_IMAGE_PREPARATION_TIMEOUT_SECS),
            Command::new(&self.engine.binary_path)
                .args(["pull", image_ref])
                .kill_on_drop(true)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await
        .map_err(|_| {
            CompassError::ContainerEngine(format!(
                "timed out pulling OCI image '{image_ref}' via {}",
                self.engine.kind.as_str()
            ))
        })?
        .map_err(|e| {
            CompassError::ContainerEngine(format!(
                "spawn {} pull failed: {e}",
                self.engine.binary_path.display()
            ))
        })?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            return Err(CompassError::ContainerEngine(format!(
                "failed to pull OCI image '{image_ref}': {}",
                if stderr.is_empty() {
                    format!("exit code {:?}", out.status.code())
                } else {
                    stderr
                }
            )));
        }
        Ok(())
    }

    /// Execute a hardened OCI container, streaming `stdout`/`stderr` lines and enforcing `timeout_sec`.
    pub async fn execute(
        &self,
        spec: &ContainerRunSpec,
        log_tx: Option<broadcast::Sender<RunLogEvent>>,
    ) -> CompassResult<ContainerExecutionReport> {
        self.execute_with_revocation(spec, log_tx, None).await
    }

    pub async fn execute_with_revocation(
        &self,
        spec: &ContainerRunSpec,
        log_tx: Option<broadcast::Sender<RunLogEvent>>,
        revoked: Option<&AtomicBool>,
    ) -> CompassResult<ContainerExecutionReport> {
        self.execute_with_queries(spec, log_tx, revoked, None).await
    }

    /// Enable an isolated request/response pipe for a run with a provider query session.
    pub async fn execute_with_queries(
        &self,
        spec: &ContainerRunSpec,
        log_tx: Option<broadcast::Sender<RunLogEvent>>,
        revoked: Option<&AtomicBool>,
        responder: Option<Arc<dyn LiveQueryResponder>>,
    ) -> CompassResult<ContainerExecutionReport> {
        self.execute_with_start_notification(spec, log_tx, revoked, responder, None).await
    }

    /// Notify the orchestrator after image preparation, when the execution deadline begins.
    pub async fn execute_with_start_notification(
        &self,
        spec: &ContainerRunSpec,
        log_tx: Option<broadcast::Sender<RunLogEvent>>,
        revoked: Option<&AtomicBool>,
        responder: Option<Arc<dyn LiveQueryResponder>>,
        started_tx: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> CompassResult<ContainerExecutionReport> {
        self.execute_with_limits(spec, log_tx, revoked, responder, started_tx, None)
            .await
    }

    /// Execute with explicit mid-run `max_output_bytes` enforcement across stdout, stderr,
    /// `BIOCOMPASS_QUERY_V1` frames, and `/out` regular files (`SEC-28`).
    pub async fn execute_with_limits(
        &self,
        spec: &ContainerRunSpec,
        log_tx: Option<broadcast::Sender<RunLogEvent>>,
        revoked: Option<&AtomicBool>,
        responder: Option<Arc<dyn LiveQueryResponder>>,
        started_tx: Option<tokio::sync::oneshot::Sender<()>>,
        max_output_bytes: Option<u64>,
    ) -> CompassResult<ContainerExecutionReport> {
        let revoked_error = || {
            CompassError::AccessDenied("DATASET_ACCESS_REVOKED: dataset access was revoked".into())
        };
        if revoked.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(revoked_error());
        }
        if responder.is_some() && !spec.network.is_airgapped() {
            return Err(CompassError::Validation(
                "live queries require an air-gapped container".into(),
            ));
        }
        let container_name = self.container_name(&spec.run_id);
        let mut args = build_hardened_oci_args_with_local_allowance(
            &self.engine,
            &container_name,
            spec,
            self.workspace_scope.is_some(),
        )?;
        self.label_native_job(&mut args);
        let wait_for_revocation = async {
            match revoked {
                Some(flag) => while !flag.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                },
                None => std::future::pending().await,
            }
        };
        tokio::pin!(wait_for_revocation);
        let _image_lease;
        if spec.image.sha256_digest.is_some() {
            _image_lease = tokio::select! {
                biased;
                () = &mut wait_for_revocation => return Err(revoked_error()),
                result = tokio::time::timeout(Duration::from_secs(OCI_IMAGE_PREPARATION_TIMEOUT_SECS),
                    self.prepare_for_execution(&spec.image.raw)) => {
                    result.map_err(|_| CompassError::ContainerEngine(format!(
                        "timed out preparing pinned OCI image '{}'", spec.image.raw
                    )))??
                }
            };
            args.insert(1, "--pull=never".into());
        } else if self.worker_cache.is_some()
            || spec.image.raw.starts_with("sha256:")
            || spec.image.repository.starts_with("local/bundled-")
        {
            // A development tag may use an already installed image. Only the
            // digest admission path can fetch images into a worker-owned engine.
            args.insert(1, "--pull=never".into());
        }
        if revoked.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(revoked_error());
        }
        let stdin = if responder.is_some() {
            args.insert(1, "--interactive".into());
            Stdio::piped()
        } else {
            Stdio::null()
        };

        let started = Instant::now();
        let mut child = Command::new(&self.engine.binary_path)
            .args(&args)
            .stdin(stdin)
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

        if let Some(tx) = started_tx {
            let _ = tx.send(());
        }
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CompassError::ContainerEngine("capture container stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| CompassError::ContainerEngine("capture container stderr".into()))?;
        let channel = match (responder, child.stdin.take()) {
            (Some(responder), Some(stdin)) => Some(LiveQueryChannel { stdin, responder }),
            (Some(_), None) => {
                let reason = CompassError::ContainerEngine("capture container stdin".into());
                return Err(self.abort_execution(&mut child, &container_name, reason).await);
            }
            (None, _) => None,
        };

        let shared_stream_bytes = Arc::new(AtomicU64::new(0));
        let out_quota_exceeded = async {
            let Some(cap) = max_output_bytes else {
                return std::future::pending::<CompassError>().await;
            };
            let out_dir = spec.out_dir.clone();
            let streamed = Arc::clone(&shared_stream_bytes);
            loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let dir_bytes = measure_out_dir_regular_bytes(&out_dir);
                let total = dir_bytes.saturating_add(streamed.load(Ordering::Acquire));
                if total > cap {
                    return CompassError::ContainerEngine(format!(
                        "OUTPUT_QUOTA_EXCEEDED: container output ({total} bytes) exceeded max_output_bytes ({cap})"
                    ));
                }
            }
        };
        tokio::pin!(out_quota_exceeded);

        let timeout_dur = Duration::from_secs(spec.resources.timeout_sec.max(1));
        let execute = async {
            let wait = async {
                child.wait().await.map_err(|e| {
                    CompassError::ContainerEngine(format!(
                        "wait on container {container_name} failed: {e}"
                    ))
                })
            };
            tokio::try_join!(
                wait,
                drain_stream_bounded(
                    stdout,
                    "stdout",
                    &spec.run_id,
                    log_tx.as_ref(),
                    channel,
                    max_output_bytes,
                    Some(&shared_stream_bytes),
                ),
                drain_stream_bounded(
                    stderr,
                    "stderr",
                    &spec.run_id,
                    log_tx.as_ref(),
                    None::<LiveQueryChannel<tokio::process::ChildStdin>>,
                    max_output_bytes,
                    Some(&shared_stream_bytes),
                ),
            )
        };
        let wait_res = tokio::select! {
            biased;
            () = &mut wait_for_revocation => {
                return Err(self.abort_execution(&mut child, &container_name, revoked_error()).await);
            }
            quota_err = &mut out_quota_exceeded => {
                return Err(self.abort_execution(&mut child, &container_name, quota_err).await);
            }
            result = tokio::time::timeout(timeout_dur, execute) => result,
        };

        match wait_res {
            Ok(Ok((status, (stdout_lines, stdout_bytes), (stderr_lines, stderr_bytes)))) => {
                let exit_code = status.code().unwrap_or(-1);
                Ok(ContainerExecutionReport {
                    engine: self.engine.kind,
                    engine_version: self.engine.server_version.clone(),
                    container_name,
                    exit_code,
                    duration_ms: started.elapsed().as_millis() as u64,
                    stdout_lines,
                    stderr_lines,
                    log_bytes_total: stdout_bytes.saturating_add(stderr_bytes),
                })
            }
            Ok(Err(error)) => Err(self.abort_execution(&mut child, &container_name, error).await),
            Err(_) => {
                let reason = CompassError::ContainerEngine(format!(
                    "CONTAINER_TIMEOUT: container {container_name} exceeded hard timeout of {}s",
                    spec.resources.timeout_sec
                ));
                Err(self.abort_execution(&mut child, &container_name, reason).await)
            }
        }
    }
}

fn measure_out_dir_regular_bytes(root: &Path) -> u64 {
    fn walk(dir: &Path, total: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_file() {
                *total = total.saturating_add(meta.len());
            } else if meta.is_dir() {
                walk(&entry.path(), total);
            }
        }
    }
    let mut total = 0u64;
    walk(root, &mut total);
    total
}

async fn drain_stream_bounded<R: tokio::io::AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    mut stream: R,
    stream_name: &'static str,
    run_id: &RunId,
    log_tx: Option<&broadcast::Sender<RunLogEvent>>,
    mut channel: Option<LiveQueryChannel<W>>,
    max_output_bytes: Option<u64>,
    shared_quota_bytes: Option<&AtomicU64>,
) -> CompassResult<(Vec<String>, u64)> {
    const MAX_RETAINED_LINES: usize = 2_000;
    const MAX_LINE_BYTES: usize = 8_192;
    const MAX_BUFFER_BYTES: usize = MAX_QUERY_REQUEST_BYTES + LIVE_QUERY_PREFIX.len();

    let mut buf = [0u8; 8_192];
    let mut current_line: Vec<u8> = Vec::new();
    let mut line_byte_count = 0u64;
    let mut lines: Vec<String> = Vec::new();
    let mut total_bytes: u64 = 0;
    let mut raw_stream_bytes: u64 = 0;

    let emit_line = |line_bytes: &[u8], lines: &mut Vec<String>| {
        let trimmed = line_bytes.strip_suffix(b"\r").unwrap_or(line_bytes);
        let text = String::from_utf8_lossy(trimmed).into_owned();
        if let Some(tx) = log_tx {
            let _ = tx.send(RunLogEvent {
                run_id: run_id.clone(),
                timestamp: Utc::now(),
                stream: stream_name.into(),
                line: text.clone(),
            });
        }
        if lines.len() < MAX_RETAINED_LINES {
            lines.push(text);
        }
    };

    loop {
        match stream.read(&mut buf).await {
            Ok(0) => break,
            Err(e) if channel.is_some() => {
                return Err(CompassError::ContainerEngine(format!(
                    "read live query stream: {e}"
                )));
            }
            Err(_) => break,
            Ok(n) => {
                // SEC-28: Account EVERY raw byte (including BIOCOMPASS_QUERY_V1 frames on stdout/stderr)
                // against `max_output_bytes` immediately mid-stream before buffering or replying.
                raw_stream_bytes = raw_stream_bytes.saturating_add(n as u64);
                let cumulative = if let Some(shared) = shared_quota_bytes {
                    shared
                        .fetch_add(n as u64, Ordering::AcqRel)
                        .saturating_add(n as u64)
                } else {
                    raw_stream_bytes
                };
                if let Some(cap) = max_output_bytes
                    && cumulative > cap
                {
                    return Err(CompassError::ContainerEngine(format!(
                        "OUTPUT_QUOTA_EXCEEDED: container {stream_name} exceeded max_output_bytes ({cap})"
                    )));
                }

                for &b in &buf[..n] {
                    if b == b'\n' {
                        if current_line.starts_with(LIVE_QUERY_MARKER) {
                            if let Some(channel) = &mut channel {
                                channel.reply(&current_line).await?;
                            } else {
                                // SEC-28: Discarded reserved-prefix lines when no live-query channel is active
                                // MUST still count toward `total_bytes` (never reported as 0 bytes).
                                total_bytes =
                                    total_bytes.saturating_add(line_byte_count.saturating_add(1));
                            }
                        } else {
                            emit_line(
                                &current_line[..current_line.len().min(MAX_LINE_BYTES)],
                                &mut lines,
                            );
                            total_bytes =
                                total_bytes.saturating_add(line_byte_count.saturating_add(1));
                        }
                        current_line.clear();
                        line_byte_count = 0;
                    } else {
                        line_byte_count = line_byte_count.saturating_add(1);
                        if current_line.len() < MAX_BUFFER_BYTES {
                            current_line.push(b);
                        } else if channel.is_some() && current_line.starts_with(LIVE_QUERY_MARKER) {
                            return Err(CompassError::ContainerEngine(
                                "live query request exceeded 8192 bytes".into(),
                            ));
                        }
                    }
                }
            }
        }
    }
    if !current_line.is_empty() {
        if current_line.starts_with(LIVE_QUERY_MARKER) {
            if channel.is_some() {
                return Err(CompassError::ContainerEngine(
                    "unterminated live query request frame".into(),
                ));
            }
            total_bytes = total_bytes.saturating_add(line_byte_count);
        } else {
            emit_line(
                &current_line[..current_line.len().min(MAX_LINE_BYTES)],
                &mut lines,
            );
            total_bytes = total_bytes.saturating_add(line_byte_count);
        }
    }
    Ok((lines, total_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoLiveQuery;

    #[async_trait::async_trait]
    impl LiveQueryResponder for EchoLiveQuery {
        async fn call(&self, frame: &[u8]) -> CompassResult<Vec<u8>> {
            Ok(frame.to_vec())
        }
    }

    #[tokio::test]
    async fn live_query_frames_round_trip_without_entering_logs() {
        let input = b"starting\nBIOCOMPASS_QUERY_V1 {\"value\": 42}\nfinished\r\n";
        let run_id = RunId::from("run-query-test");
        let (tx, mut rx) = broadcast::channel(8);
        let (pipe, mut module_stdin) = tokio::io::duplex(128);
        let (lines, bytes) = drain_stream_bounded(
            &input[..],
            "stdout",
            &run_id,
            Some(&tx),
            Some(LiveQueryChannel {
                stdin: pipe,
                responder: Arc::new(EchoLiveQuery),
            }),
            None,
            None,
        )
        .await
        .expect("drain stdout");
        let mut response = Vec::new();
        module_stdin
            .read_to_end(&mut response)
            .await
            .expect("read exact response");
        assert_eq!(response, b"BIOCOMPASS_QUERY_V1 {\"value\": 42}\n");
        assert_eq!(lines, ["starting", "finished"]);
        assert_eq!(bytes, b"starting\nfinished\r\n".len() as u64);
        assert_eq!(rx.try_recv().expect("first log").line, "starting");
        assert_eq!(rx.try_recv().expect("second log").line, "finished");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn bounded_live_query_frames_are_never_emitted_as_truncated_logs() {
        let run_id = RunId::from("run-query-test");
        let (tx, mut rx) = broadcast::channel(8);
        let mut oversize = LIVE_QUERY_PREFIX.to_vec();
        oversize.extend(std::iter::repeat_n(b'x', MAX_QUERY_REQUEST_BYTES + 1));
        let err = drain_stream_bounded(
            &oversize[..],
            "stdout",
            &run_id,
            Some(&tx),
            Some(LiveQueryChannel {
                stdin: tokio::io::sink(),
                responder: Arc::new(EchoLiveQuery),
            }),
            None,
            None,
        )
        .await
        .expect_err("reject oversized query before newline");
        assert!(err.to_string().contains("exceeded 8192 bytes"));
        assert!(rx.try_recv().is_err());

        let (lines, bytes) = drain_stream_bounded(
            &oversize[..],
            "stderr",
            &run_id,
            Some(&tx),
            None::<LiveQueryChannel<tokio::io::Sink>>,
            None,
            None,
        )
        .await
        .expect("discard a reserved frame from stderr");
        assert!(lines.is_empty());
        assert_eq!(bytes, oversize.len() as u64);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn sec_28_unconditional_log_driver_none_and_max_output_bytes_across_live_query_frames() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = DetectedEngine {
            kind: EngineKind::Docker,
            binary_path: PathBuf::from("/usr/bin/docker"),
            server_version: "29.10.0".into(),
            rootless: false,
        };
        let spec = ContainerRunSpec {
            run_id: RunId::from("run-sec-28"),
            image: OciImageRef::parse("local/test:dev", true).unwrap(),
            in_dir: tmp.path().join("in"),
            out_dir: tmp.path().join("out"),
            resources: PluginResources {
                cpus: 1,
                memory_mb: 128,
                gpu: 0,
                timeout_sec: 10,
                pids_limit: 32,
            },
            network: NetworkPolicy::default(),
            command_override: None,
        };
        let args = build_hardened_oci_args(&engine, "bc-sec-28", &spec).unwrap();
        assert!(
            args.iter().any(|a| a == "--log-driver=none"),
            "SEC-28: --log-driver=none must be unconditional: {args:?}"
        );

        let run_id = RunId::from("run-sec-28");
        let flood_line = b"BIOCOMPASS_QUERY_V1 {\"flood\":\"payload-payload-payload\"}\n";
        let mut flood = Vec::new();
        for _ in 0..20 {
            flood.extend_from_slice(flood_line);
        }

        // 1. Without live-query channel (e.g. stderr / normal stdout), BIOCOMPASS_QUERY_V1 flood is counted & rejected!
        let (lines, counted) = drain_stream_bounded(
            &flood[..],
            "stderr",
            &run_id,
            None,
            None::<LiveQueryChannel<tokio::io::Sink>>,
            None,
            None,
        )
        .await
        .expect("drain without cap counts all bytes");
        assert!(lines.is_empty());
        assert_eq!(counted, flood.len() as u64);

        let err = drain_stream_bounded(
            &flood[..],
            "stderr",
            &run_id,
            None,
            None::<LiveQueryChannel<tokio::io::Sink>>,
            Some(256),
            None,
        )
        .await
        .expect_err("SEC-28 must reject BIOCOMPASS_QUERY_V1 flood over max_output_bytes");
        assert!(err.to_string().contains("OUTPUT_QUOTA_EXCEEDED"));

        // 2. Even WITH an active live-query channel, BIOCOMPASS_QUERY_V1 frames obey max_output_bytes!
        let err_live = drain_stream_bounded(
            &flood[..],
            "stdout",
            &run_id,
            None,
            Some(LiveQueryChannel {
                stdin: tokio::io::sink(),
                responder: Arc::new(EchoLiveQuery),
            }),
            Some(256),
            None,
        )
        .await
        .expect_err("SEC-28 must enforce max_output_bytes across live-query frames too");
        assert!(err_live.to_string().contains("OUTPUT_QUOTA_EXCEEDED"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn revocation_and_deadline_cancel_inflight_live_queries_and_remove_container() {
        use std::os::unix::fs::PermissionsExt;
        use tokio::sync::Notify;

        struct PendingQuery {
            called: Notify,
            dropped: AtomicBool,
        }

        #[async_trait::async_trait]
        impl LiveQueryResponder for PendingQuery {
            async fn call(&self, _: &[u8]) -> CompassResult<Vec<u8>> {
                struct DropFlag<'a>(&'a AtomicBool);
                impl Drop for DropFlag<'_> {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::Release);
                    }
                }
                let _drop_flag = DropFlag(&self.dropped);
                self.called.notify_one();
                std::future::pending().await
            }
        }

        let tmp = tempfile::tempdir().expect("fixture dir");
        let binary_path = tmp.path().join("fake-docker");
        let removal = tmp.path().join("removed");
        std::fs::write(
            &binary_path,
            r#"#!/bin/sh
case "$1" in
  run)
    [ "$2" = --interactive ] || exit 42
    for arg; do
      case "$arg" in
        --network=none) offline=1 ;;
        --log-driver=none) no_logs=1 ;;
        --tty|-t) exit 43 ;;
      esac
    done
    [ "$offline" = 1 ] && [ "$no_logs" = 1 ] || exit 44
    printf 'BIOCOMPASS_QUERY_V1 {}\n'
    read -r response
    ;;
  rm) printf '%s\n' "$3" > "${0%/*}/removed" ;;
  *) exit 1 ;;
esac
"#,
        )
        .expect("write engine fixture");
        std::fs::set_permissions(&binary_path, std::fs::Permissions::from_mode(0o700))
            .expect("enable engine fixture");
        let runner = OciContainerRunner::from_engine(DetectedEngine {
            kind: EngineKind::Docker,
            binary_path,
            server_version: "test".into(),
            rootless: false,
        });
        let mut spec = ContainerRunSpec {
            run_id: RunId::from("run-query-test"),
            image: OciImageRef::parse("local/test:one", true).expect("image"),
            in_dir: tmp.path().join("in"),
            out_dir: tmp.path().join("out"),
            resources: PluginResources {
                cpus: 1,
                memory_mb: 128,
                gpu: 0,
                timeout_sec: 1,
                pids_limit: 32,
            },
            network: NetworkPolicy::default(),
            command_override: None,
        };
        for expected in ["CONTAINER_TIMEOUT", "DATASET_ACCESS_REVOKED"] {
            let responder = Arc::new(PendingQuery {
                called: Notify::new(),
                dropped: AtomicBool::new(false),
            });
            let revoked = AtomicBool::new(false);
            if expected == "DATASET_ACCESS_REVOKED" {
                spec.resources.timeout_sec = 10;
            }
            let revoke = async {
                tokio::time::timeout(Duration::from_secs(2), responder.called.notified())
                    .await
                    .expect("module requests query");
                if expected == "DATASET_ACCESS_REVOKED" {
                    revoked.store(true, Ordering::Release);
                }
            };
            let (execution, ()) = tokio::join!(
                runner.execute_with_queries(&spec, None, Some(&revoked), Some(responder.clone())),
                revoke,
            );
            assert!(
                execution
                    .expect_err("execution cancelled")
                    .to_string()
                    .contains(expected)
            );
            assert!(responder.dropped.load(Ordering::Acquire));
            assert!(
                std::fs::read_to_string(&removal)
                    .expect("container removed")
                    .starts_with("biocompass-job-")
            );
            std::fs::remove_file(&removal).expect("clear removal marker");
        }
    }

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

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn sec_12_non_airgapped_network_rejected_outside_local_mode_and_warns_in_local_mode() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let engine_bin = tmp.path().join("fake-docker");
        std::fs::write(
            &engine_bin,
            r#"#!/bin/sh
if [ "$1" = "run" ]; then
  exit 0
fi
exit 0
"#,
        )
        .unwrap();
        std::fs::set_permissions(&engine_bin, std::fs::Permissions::from_mode(0o700)).unwrap();

        let engine = DetectedEngine {
            kind: EngineKind::Docker,
            binary_path: engine_bin,
            server_version: "29.10.0".into(),
            rootless: false,
        };
        let in_dir = tmp.path().join("in");
        let out_dir = tmp.path().join("out");
        std::fs::create_dir_all(&in_dir).unwrap();
        std::fs::create_dir_all(&out_dir).unwrap();

        let spec = ContainerRunSpec {
            run_id: RunId::from("run-sec-12"),
            image: OciImageRef::parse("local/test:dev", true).unwrap(),
            in_dir,
            out_dir,
            resources: PluginResources {
                cpus: 1,
                memory_mb: 256,
                gpu: 0,
                timeout_sec: 10,
                pids_limit: 32,
            },
            network: NetworkPolicy::Allowlist {
                allow_domains: vec!["eutils.ncbi.nlm.nih.gov".into()],
            },
            command_override: None,
        };

        // 1. Default cloud/edge build_hardened_oci_args and non-workspace runner MUST reject non-airgapped!
        let err = build_hardened_oci_args(&engine, "biocompass-cloud-job", &spec)
            .expect_err("cloud/edge must reject allow_domains / non-airgapped execution");
        assert!(matches!(err, CompassError::AccessDenied(_)));

        let cloud_runner = OciContainerRunner::from_engine(engine.clone());
        let exec_err = cloud_runner
            .execute(&spec, None)
            .await
            .expect_err("cloud/edge runner must reject non-airgapped execution");
        assert!(matches!(exec_err, CompassError::AccessDenied(_)));

        // 2. Local native workspace runner permits allow_domains -> --network=bridge and emits a WARN.
        let stage_root = tmp.path().join("stages");
        std::fs::create_dir_all(&stage_root).unwrap();
        let local_runner = cloud_runner
            .for_native_workspace(&stage_root, "provider-pubkey")
            .unwrap();
        let buf = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let writer_buf = Arc::clone(&buf);
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .without_time()
            .with_writer(move || SharedVecWriter(Arc::clone(&writer_buf)))
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let report = local_runner
            .execute(&spec, None)
            .await
            .expect("local workstation runner permits allow_domains with warning");
        assert_eq!(report.exit_code, 0);
        let logs = String::from_utf8_lossy(&buf.lock().unwrap()).into_owned();
        assert!(
            logs.contains("WARN") && logs.contains("--network=bridge"),
            "expected local bridge egress warning, got: {logs}"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn sec_13_linux_docker_fallback_emits_startup_warning() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let missing_podman = tmp.path().join("no-podman");
        let fake_docker = tmp.path().join("fake-docker");
        std::fs::write(
            &fake_docker,
            "#!/bin/sh\nif [ \"$1\" = \"info\" ]; then printf '29.10.0|[name=seccomp]\\n'; exit 0; fi\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_docker, std::fs::Permissions::from_mode(0o700)).unwrap();

        let buf = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let writer_buf = Arc::clone(&buf);
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .without_time()
            .with_writer(move || SharedVecWriter(Arc::clone(&writer_buf)))
            .finish();
        let detected = {
            let _guard = tracing::subscriber::set_default(subscriber);
            detect_engine_with_candidates(
                true,
                &[missing_podman.to_str().unwrap()],
                &[fake_docker.to_str().unwrap()],
            )
            .await
            .expect("detect standard docker fallback on Linux")
        };
        assert_eq!(detected.kind, EngineKind::Docker);
        assert!(!detected.rootless);
        let logs = String::from_utf8_lossy(&buf.lock().unwrap()).into_owned();
        assert!(
            logs.contains("WARN") && logs.to_ascii_lowercase().contains("podman") && logs.to_ascii_lowercase().contains("docker"),
            "expected Linux Podman->Docker fallback WARN, got: {logs}"
        );
    }

    struct SharedVecWriter(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for SharedVecWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
