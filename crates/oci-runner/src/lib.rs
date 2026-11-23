//! `biocompass-oci-runner`: Hardened container execution seam (`ContainerRunner` + `supervise`),
//! rootless Podman / VM-isolated Docker OCI runner (`OciContainerRunner` / `LocalOciRunner`),
//! hardened host-container file mailbox (`mailbox`), and OCI Distribution v1.1 manifest verifier.

pub mod mailbox;
pub mod registry;
mod cleanup;
pub use cleanup::AbandonedContainer;
mod release_archive;
mod worker_cache;
#[cfg(all(test, unix))]
mod image_preparation_tests;
#[cfg(all(test, unix))]
mod cleanup_tests;
#[cfg(test)]
mod mailbox_tests;

use biocompass_core_traits::{CompassError, CompassResult, ErrorCode, OciImageRef, RunId, RunLogEvent};
use biocompass_core_traits::types::OCI_IMAGE_PREPARATION_TIMEOUT_SECS;
use biocompass_plugin_spec::{MAX_CHILD_WAIT_SEC, NetworkPolicy, PluginResources};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::broadcast;
use uuid::Uuid;

pub use mailbox::{
    CONTAINER_ARTIFACTS_DIR, CONTAINER_MAILBOX_REPLIES_DIR, CONTAINER_MAILBOX_REQUESTS_DIR,
    ChildModuleRequest, ChildRunBroker, ChildRunBrokerContext, ChildRunReply,
    ChildRunStagedArtifact, DEFAULT_MAX_MAILBOX_CALLS, FileMailbox, LiveQueryMailboxRequest,
    LiveQueryResponder, MAILBOX_CONFIG_REL, MAILBOX_REPLIES_REL, MAILBOX_REQUESTS_REL,
    MAILBOX_ROOT_REL, MAX_LIVE_QUERY_REQUEST_BYTES, MAX_MAILBOX_REPLY_BYTES,
    MAX_MAILBOX_REQUEST_BYTES, MailboxConfig, MailboxRequest, child_artifact_alias,
    filesystem_free_bytes, is_valid_request_id, prepare_mailbox_dirs, stage_child_artifact_bytes,
};
pub use registry::OciDistributionClient;

/// Default cumulative pause allowance (seconds) for pending child module runs when a parent
/// manifest omits `max_child_wait_sec` (`W7`).
pub const DEFAULT_MAX_CHILD_WAIT_SEC: u64 = 3_600;
/// Default supervision tick interval for quota, mailbox, and pause-aware deadline checks (`W2`).
pub const DEFAULT_SUPERVISE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Detected host OCI runtime CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedEngine {
    pub kind: EngineKind,
    pub binary_path: PathBuf,
    pub server_version: String,
    pub rootless: bool,
}

/// Supported container execution backends (`W1`, `W3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    Podman,
    Docker,
    Kubernetes,
}

impl EngineKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Podman => "podman",
            Self::Docker => "docker",
            Self::Kubernetes => "kubernetes",
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

/// Snapshot of `/out` directory usage returned by [`ContainerRunner::inspect_out_dir`] (`W1`, `W3`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutDirStats {
    /// Total regular-file bytes under `/out`.
    pub regular_bytes: u64,
    /// Total filesystem entries (files, directories, symlinks, specials) under `/out`.
    pub entry_count: u64,
    /// Available bytes on the filesystem backing `/out`, when reported by the OS.
    pub free_bytes: Option<u64>,
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
    /// Signed Froglet receipt hashes of child runs completed via the file mailbox (`W7`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_receipt_hashes: Vec<String>,
    /// Cumulative milliseconds spent paused while awaiting brokered child runs (`W7`).
    #[serde(default)]
    pub child_wait_ms: u64,
}

/// Live execution handle returned by [`ContainerRunner::spawn`] (`W1`).
#[async_trait::async_trait]
pub trait ContainerHandle: Send {
    /// Backend container or Pod identifier recorded in [`ContainerExecutionReport::container_name`].
    fn container_name(&self) -> &str;

    /// Take the container stdout stream (called once by [`supervise`]).
    /// Backends with combined log streams (e.g. Kubernetes pods) surface combined logs here.
    fn take_stdout(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>>;

    /// Take the container stderr stream (called once by [`supervise`]).
    /// Returns `None` for backends with combined log streams.
    fn take_stderr(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>>;

    /// Wait for the container / Pod to exit and return its exit code.
    ///
    /// Must be cancel-safe: [`supervise`] may drop this future on timeout, revocation,
    /// output quota breach, mailbox violation, or cancellation, and immediately call [`Self::kill`].
    async fn wait(&mut self) -> CompassResult<i32>;

    /// Force-terminate and remove the running container / Pod and any attached client process.
    /// Returns `Ok(())` when removal is confirmed, or an error describing unconfirmed cleanup.
    async fn kill(&mut self) -> CompassResult<()>;
}

/// Cloud-agnostic container execution backend (`W1`).
///
/// Implemented by [`OciContainerRunner`] (alias [`LocalOciRunner`]) for local Podman/Docker
/// and by `biocompass-kube-runner::KubeRunner` for unprivileged Kubernetes Run Pods (`W3`).
#[async_trait::async_trait]
pub trait ContainerRunner: Send + Sync {
    /// Backend engine kind (`podman`, `docker`, or `kubernetes`).
    fn engine_kind(&self) -> EngineKind;

    /// Backend version string recorded in Froglet receipts.
    fn engine_version(&self) -> String;

    /// Whether this runner permits local workstation `allow_domains` bridge egress.
    /// Hosted / cloud / Kubernetes / edge runners MUST return `false` (`SEC-12`).
    fn allows_local_bridge_egress(&self) -> bool {
        false
    }

    /// Optional pre-execution image preparation (e.g. cold digest pull / worker image cache lease)
    /// executed before the signed container execution deadline begins.
    /// Any returned guard is held alive until [`supervise`] finishes.
    async fn prepare_image(
        &self,
        _spec: &ContainerRunSpec,
    ) -> CompassResult<Option<Box<dyn Any + Send + Sync>>> {
        Ok(None)
    }

    /// Spawn a hardened container / Run Pod for `spec` and return its live [`ContainerHandle`].
    async fn spawn(&self, spec: &ContainerRunSpec) -> CompassResult<Box<dyn ContainerHandle>>;

    /// Inspect the `/out` stage directory for regular-file bytes, entry count, and free space.
    async fn inspect_out_dir(&self, out_dir: &Path) -> CompassResult<OutDirStats> {
        Ok(measure_out_dir_stats(out_dir))
    }

    /// Cancel / force-delete any active container or Pod associated with `run_id` (`W5`).
    async fn cancel(&self, run_id: &RunId) -> CompassResult<()>;

    /// Remove a run's stage directory (`runs/<id>`), allowing Kubernetes runners to fall back
    /// to an unprivileged UID-65532 cleanup pod when cross-UID `unlink` fails (`W5`).
    async fn cleanup_stage_dir(&self, stage_dir: &Path) -> CompassResult<()> {
        if stage_dir.exists() {
            std::fs::remove_dir_all(stage_dir).map_err(|e| {
                CompassError::Storage(format!("remove stage dir {}: {e}", stage_dir.display()))
            })?;
        }
        Ok(())
    }
}

/// Configuration options for [`supervise`] (`W1`, `W2`, `W3`, `W5`, `W7`).
pub struct SuperviseOptions<'a> {
    pub log_tx: Option<broadcast::Sender<RunLogEvent>>,
    pub revoked: Option<&'a AtomicBool>,
    pub cancelled: Option<&'a AtomicBool>,
    pub live_query: Option<Arc<dyn LiveQueryResponder>>,
    pub child_broker: Option<Arc<dyn ChildRunBroker>>,
    /// Allow-listed child module IDs (`<org>/<name>`) permitted for `"run_module"` requests (`W7`).
    pub allowed_child_modules: Vec<String>,
    /// Maximum cumulative seconds the parent wall clock may pause while child runs are pending (`W7`).
    pub max_child_wait_sec: Option<u64>,
    /// Cap on total file-mailbox requests processed for this run (`W2`).
    pub max_mailbox_calls: usize,
    /// Optional oneshot notified immediately after image preparation and container spawn, when the
    /// signed execution deadline begins.
    pub started_tx: Option<tokio::sync::oneshot::Sender<()>>,
    /// Mid-run output byte cap across `/out` regular files + stdout/stderr (`SEC-28`).
    pub max_output_bytes: Option<u64>,
    /// Optional mid-run `/out` entry-count cap (`W3`).
    pub max_output_entries: Option<u64>,
    /// Optional minimum free bytes watchdog on the filesystem backing `/out` (`W3`).
    pub min_free_bytes: Option<u64>,
    /// Supervision poll interval for quota, mailbox, and pause-aware deadline checks (default 50 ms).
    pub poll_interval: Duration,
}

impl Default for SuperviseOptions<'_> {
    fn default() -> Self {
        Self {
            log_tx: None,
            revoked: None,
            cancelled: None,
            live_query: None,
            child_broker: None,
            allowed_child_modules: Vec::new(),
            max_child_wait_sec: None,
            max_mailbox_calls: DEFAULT_MAX_MAILBOX_CALLS,
            started_tx: None,
            max_output_bytes: None,
            max_output_entries: None,
            min_free_bytes: None,
            poll_interval: DEFAULT_SUPERVISE_POLL_INTERVAL,
        }
    }
}

enum InFlightOutcome {
    LiveQuery {
        request_id: String,
        result: CompassResult<Vec<u8>>,
    },
    ChildRun {
        request_id: String,
        result: CompassResult<ChildRunReply>,
    },
}

type InFlightFuture = Pin<Box<dyn Future<Output = InFlightOutcome> + Send>>;

/// Shared execution supervision loop for EVERY [`ContainerRunner`] backend (`W1`, `W2`, `W3`, `W5`, `W7`).
///
/// Enforces:
/// - Airgap / local-bridge network policy (`SEC-12`)
/// - Pre-execution image preparation raced against revocation / cancellation (not billed to deadline)
/// - Bounded stdout/stderr draining & `SEC-28` raw-byte `max_output_bytes` enforcement
/// - Periodic (50 ms) `/out` regular-byte cap, entry-count cap, and free-space watchdog
/// - Hardened `O_NOFOLLOW | O_NONBLOCK` file mailbox for live queries (`W2`) and brokered child runs (`W7`)
/// - Pause-aware parent wall-clock deadline that pauses while child runs are pending, capped by `max_child_wait_sec`
/// - Guaranteed container/Pod force-kill on any violation, timeout, revocation, or cancellation
pub async fn supervise(
    runner: &dyn ContainerRunner,
    spec: &ContainerRunSpec,
    opts: SuperviseOptions<'_>,
) -> CompassResult<ContainerExecutionReport> {
    let revoked_error = || {
        CompassError::AccessDenied("DATASET_ACCESS_REVOKED: dataset access was revoked".into())
    };
    let cancelled_error = || CompassError::ContainerEngine("RUN_CANCELLED: run was cancelled".into());

    if opts.revoked.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(revoked_error());
    }
    if opts.cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(cancelled_error());
    }
    if opts.live_query.is_some() && !spec.network.is_airgapped() {
        return Err(CompassError::Validation(
            "live queries require an air-gapped container".into(),
        ));
    }
    if !spec.network.is_airgapped() && !runner.allows_local_bridge_egress() {
        return Err(CompassError::AccessDenied(
            "non-airgapped container network policy is rejected outside local mode (--network=none required)".into(),
        ));
    }

    let enable_mailbox = opts.live_query.is_some()
        || opts.child_broker.is_some()
        || !opts.allowed_child_modules.is_empty();
    let mailbox = if enable_mailbox {
        let cfg = MailboxConfig {
            run_id: spec.run_id.clone(),
            in_dir: spec.in_dir.clone(),
            out_dir: spec.out_dir.clone(),
            live_query: opts.live_query.clone(),
            child_broker: opts.child_broker.clone(),
            allowed_child_modules: opts.allowed_child_modules.clone(),
            max_child_wait_sec: opts.max_child_wait_sec,
            max_calls: opts.max_mailbox_calls,
        };
        Some(Arc::new(std::sync::Mutex::new(FileMailbox::open(cfg)?)))
    } else {
        None
    };

    let wait_for_revocation = wait_for_flag(opts.revoked);
    let wait_for_cancel = wait_for_flag(opts.cancelled);
    tokio::pin!(wait_for_revocation);
    tokio::pin!(wait_for_cancel);

    let _image_lease = tokio::select! {
        biased;
        () = &mut wait_for_revocation => return Err(revoked_error()),
        () = &mut wait_for_cancel => return Err(cancelled_error()),
        result = tokio::time::timeout(
            Duration::from_secs(OCI_IMAGE_PREPARATION_TIMEOUT_SECS),
            runner.prepare_image(spec),
        ) => {
            result.map_err(|_| CompassError::ContainerEngine(format!(
                "timed out preparing OCI image '{}'",
                spec.image.raw
            )))??
        }
    };

    if opts.revoked.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(revoked_error());
    }
    if opts.cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(cancelled_error());
    }

    let mut handle = tokio::select! {
        biased;
        () = &mut wait_for_revocation => {
            let _ = runner.cancel(&spec.run_id).await;
            return Err(revoked_error());
        }
        () = &mut wait_for_cancel => {
            let _ = runner.cancel(&spec.run_id).await;
            return Err(cancelled_error());
        }
        spawned = runner.spawn(spec) => spawned?,
    };

    let started = Instant::now();
    if let Some(tx) = opts.started_tx {
        let _ = tx.send(());
    }

    let container_name = handle.container_name().to_owned();
    let stdout = handle.take_stdout();
    let stderr = handle.take_stderr();
    let shared_stream_bytes = Arc::new(AtomicU64::new(0));
    let child_wait_ms_shared = Arc::new(AtomicU64::new(0));

    let parent_timeout = Duration::from_secs(spec.resources.timeout_sec.max(1));
    let max_child_wait = Duration::from_secs(
        opts.max_child_wait_sec
            .unwrap_or(DEFAULT_MAX_CHILD_WAIT_SEC)
            .min(MAX_CHILD_WAIT_SEC),
    );
    let poll_interval = if opts.poll_interval.is_zero() {
        DEFAULT_SUPERVISE_POLL_INTERVAL
    } else {
        opts.poll_interval
    };

    let watchdog_container_name = container_name.clone();
    let watchdog_run_id = spec.run_id.clone();
    let watchdog_in_dir = spec.in_dir.clone();
    let watchdog_out_dir = spec.out_dir.clone();
    let watchdog_stream_bytes = Arc::clone(&shared_stream_bytes);
    let watchdog_mailbox = mailbox.clone();
    let watchdog_child_wait_ms = Arc::clone(&child_wait_ms_shared);
    let watchdog_timeout_sec = spec.resources.timeout_sec;
    let max_output_bytes = opts.max_output_bytes;
    let max_output_entries = opts.max_output_entries;
    let min_free_bytes = opts.min_free_bytes;

    let watchdog = async move {
        let mut ticker = tokio::time::interval(poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Consume the immediate first tick so first check happens after `poll_interval`
        // (except mailbox check which we can run right after first tick or immediately).
        let mut last_tick = started;
        let mut parent_elapsed = Duration::ZERO;
        let mut child_wait_elapsed = Duration::ZERO;
        let mut in_flight: Option<InFlightFuture> = None;
        let mut in_flight_is_child = false;

        let check_clocks = |parent_elapsed: Duration,
                            child_wait_elapsed: Duration,
                            watchdog_child_wait_ms: &AtomicU64|
         -> CompassResult<()> {
            watchdog_child_wait_ms.store(child_wait_elapsed.as_millis() as u64, Ordering::Release);
            if child_wait_elapsed > max_child_wait {
                return Err(CompassError::ContainerEngine(format!(
                    "CONTAINER_TIMEOUT: container {watchdog_container_name} exceeded signed max_child_wait_sec ({}s)",
                    max_child_wait.as_secs()
                )));
            }
            if parent_elapsed >= parent_timeout {
                return Err(CompassError::ContainerEngine(format!(
                    "CONTAINER_TIMEOUT: container {watchdog_container_name} exceeded hard timeout of {watchdog_timeout_sec}s"
                )));
            }
            Ok(())
        };

        loop {
            tokio::select! {
                biased;
                outcome = async {
                    in_flight
                        .as_mut()
                        .expect("guarded by in_flight.is_some()")
                        .await
                }, if in_flight.is_some() => {
                    let now = Instant::now();
                    let delta = now.saturating_duration_since(last_tick);
                    last_tick = now;
                    if in_flight_is_child {
                        child_wait_elapsed = child_wait_elapsed.saturating_add(delta);
                    } else {
                        parent_elapsed = parent_elapsed.saturating_add(delta);
                    }
                    in_flight = None;
                    in_flight_is_child = false;
                    if let Err(err) = check_clocks(parent_elapsed, child_wait_elapsed, &watchdog_child_wait_ms) {
                        return err;
                    }

                    let Some(mb_arc) = &watchdog_mailbox else {
                        return CompassError::Internal("mailbox disappeared during in-flight request".into());
                    };
                    match outcome {
                        InFlightOutcome::LiveQuery { request_id, result } => {
                            let reply_bytes = match result {
                                Ok(bytes) => bytes,
                                Err(err) => return err,
                            };
                            let write_res = {
                                let mb = mb_arc.lock().expect("mailbox mutex poisoned");
                                mb.write_reply_bytes(&request_id, &reply_bytes)
                            };
                            if let Err(err) = write_res {
                                return err;
                            }
                        }
                        InFlightOutcome::ChildRun { request_id, result } => {
                            let reply = match result {
                                Ok(mut r) => {
                                    r.request_id = request_id.clone();
                                    r
                                }
                                Err(err) => {
                                    if err.to_string().contains("DATASET_ACCESS_REVOKED") {
                                        return err;
                                    }
                                    ChildRunReply::error(request_id.clone(), err.code(), err.to_string())
                                }
                            };
                            let write_res = {
                                let mut mb = mb_arc.lock().expect("mailbox mutex poisoned");
                                if let Some(rh) = &reply.receipt_hash {
                                    mb.record_child_receipt_hash(rh.clone());
                                }
                                mb.write_child_reply(&reply)
                            };
                            if let Err(err) = write_res {
                                return err;
                            }
                        }
                    }
                }
                _ = ticker.tick() => {
                    let now = Instant::now();
                    let delta = now.saturating_duration_since(last_tick);
                    last_tick = now;
                    if in_flight_is_child {
                        child_wait_elapsed = child_wait_elapsed.saturating_add(delta);
                    } else {
                        parent_elapsed = parent_elapsed.saturating_add(delta);
                    }
                    if let Err(err) = check_clocks(parent_elapsed, child_wait_elapsed, &watchdog_child_wait_ms) {
                        return err;
                    }

                    // Enforce /out byte cap, entry-count cap, and free-space watchdog.
                    if max_output_bytes.is_some()
                        || max_output_entries.is_some()
                        || min_free_bytes.is_some()
                    {
                        let stats = match runner.inspect_out_dir(&watchdog_out_dir).await {
                            Ok(s) => s,
                            Err(err) => return err,
                        };
                        if let Some(cap) = max_output_bytes {
                            let total = stats
                                .regular_bytes
                                .saturating_add(watchdog_stream_bytes.load(Ordering::Acquire));
                            if total > cap {
                                return CompassError::ContainerEngine(format!(
                                    "OUTPUT_QUOTA_EXCEEDED: container output ({total} bytes) exceeded max_output_bytes ({cap})"
                                ));
                            }
                        }
                        if let Some(max_entries) = max_output_entries
                            && stats.entry_count > max_entries
                        {
                            return CompassError::ContainerEngine(format!(
                                "OUTPUT_QUOTA_EXCEEDED: container /out entry count ({}) exceeded max_output_entries ({max_entries})",
                                stats.entry_count
                            ));
                        }
                        if let Some(min_free) = min_free_bytes
                            && let Some(free) = stats.free_bytes
                            && free < min_free
                        {
                            return CompassError::ContainerEngine(format!(
                                "OUTPUT_QUOTA_EXCEEDED: stage filesystem free space ({free} bytes) fell below min_free_bytes ({min_free})"
                            ));
                        }
                    }

                    // Poll file mailbox when no request is currently in-flight.
                    if in_flight.is_none()
                        && let Some(mb_arc) = &watchdog_mailbox
                    {
                        let next_req = {
                            let mut mb = mb_arc.lock().expect("mailbox mutex poisoned");
                            mb.poll_next_request()
                        };
                        match next_req {
                            Err(err) => return err,
                            Ok(None) => {}
                            Ok(Some(MailboxRequest::LiveQuery(q))) => {
                                let responder = {
                                    let mb = mb_arc.lock().expect("mailbox mutex poisoned");
                                    mb.config().live_query.clone()
                                };
                                let Some(responder) = responder else {
                                    return CompassError::ContainerEngine(
                                        "MAILBOX_VIOLATION: live query request rejected because live queries are not enabled for this run".into(),
                                    );
                                };
                                let frame = match serde_json::to_vec(&q) {
                                    Ok(f) => f,
                                    Err(e) => {
                                        return CompassError::Internal(format!(
                                            "encode live query frame: {e}"
                                        ));
                                    }
                                };
                                if frame.len() > MAX_LIVE_QUERY_REQUEST_BYTES {
                                    return CompassError::ContainerEngine(format!(
                                        "MAILBOX_VIOLATION: live query request exceeded {MAX_LIVE_QUERY_REQUEST_BYTES} bytes"
                                    ));
                                }
                                let req_id = q.request_id.clone();
                                in_flight = Some(Box::pin(async move {
                                    let result = responder.call(&frame).await;
                                    InFlightOutcome::LiveQuery {
                                        request_id: req_id,
                                        result,
                                    }
                                }));
                                in_flight_is_child = false;
                            }
                            Ok(Some(MailboxRequest::ChildModule(req))) => {
                                let (broker, allowed) = {
                                    let mb = mb_arc.lock().expect("mailbox mutex poisoned");
                                    (
                                        mb.config().child_broker.clone(),
                                        mb.is_child_module_allowed(&req.plugin_id),
                                    )
                                };
                                let Some(broker) = broker else {
                                    return CompassError::child_module_not_allowed(format!(
                                        "child module '{}' rejected: child runs are not enabled for this module",
                                        req.plugin_id
                                    ));
                                };
                                if !allowed {
                                    let reply = ChildRunReply::error(
                                        req.request_id.clone(),
                                        ErrorCode::ChildModuleNotAllowed.as_str(),
                                        format!(
                                            "child module '{}' is not declared in this module's child_modules allow-list",
                                            req.plugin_id
                                        ),
                                    );
                                    let write_res = {
                                        let mb = mb_arc.lock().expect("mailbox mutex poisoned");
                                        mb.write_child_reply(&reply)
                                    };
                                    if let Err(err) = write_res {
                                        return err;
                                    }
                                    continue;
                                }
                                let remaining = max_child_wait.saturating_sub(child_wait_elapsed);
                                if remaining.is_zero() {
                                    return CompassError::ContainerEngine(format!(
                                        "CONTAINER_TIMEOUT: container {watchdog_container_name} exhausted signed max_child_wait_sec ({}s)",
                                        max_child_wait.as_secs()
                                    ));
                                }
                                let ctx = ChildRunBrokerContext {
                                    parent_run_id: watchdog_run_id.clone(),
                                    parent_in_dir: watchdog_in_dir.clone(),
                                    remaining_child_wait_sec: remaining.as_secs().max(1),
                                };
                                let req_id = req.request_id.clone();
                                in_flight = Some(Box::pin(async move {
                                    let result = broker.run_child(&ctx, req).await;
                                    InFlightOutcome::ChildRun {
                                        request_id: req_id,
                                        result,
                                    }
                                }));
                                in_flight_is_child = true;
                            }
                        }
                    }
                }
            }
        }
    };

    let execute = async {
        tokio::try_join!(
            handle.wait(),
            drain_optional_stream(
                stdout,
                "stdout",
                &spec.run_id,
                opts.log_tx.as_ref(),
                max_output_bytes,
                Some(&shared_stream_bytes),
            ),
            drain_optional_stream(
                stderr,
                "stderr",
                &spec.run_id,
                opts.log_tx.as_ref(),
                max_output_bytes,
                Some(&shared_stream_bytes),
            ),
        )
    };

    let outcome = tokio::select! {
        biased;
        () = &mut wait_for_revocation => Err(revoked_error()),
        () = &mut wait_for_cancel => Err(cancelled_error()),
        watchdog_err = watchdog => Err(watchdog_err),
        res = execute => res,
    };

    match outcome {
        Ok((exit_code, (stdout_lines, stdout_bytes), (stderr_lines, stderr_bytes))) => {
            let child_receipt_hashes = if let Some(mb_arc) = &mailbox {
                let mb = mb_arc.lock().expect("mailbox mutex poisoned");
                mb.verify_dir_integrity()?;
                mb.child_receipt_hashes()
            } else {
                Vec::new()
            };
            Ok(ContainerExecutionReport {
                engine: runner.engine_kind(),
                engine_version: runner.engine_version(),
                container_name,
                exit_code,
                duration_ms: started.elapsed().as_millis() as u64,
                stdout_lines,
                stderr_lines,
                log_bytes_total: stdout_bytes.saturating_add(stderr_bytes),
                child_receipt_hashes,
                child_wait_ms: child_wait_ms_shared.load(Ordering::Acquire),
            })
        }
        Err(reason) => Err(abort_handle(&mut *handle, reason).await),
    }
}

async fn wait_for_flag(flag: Option<&AtomicBool>) {
    match flag {
        Some(f) => {
            while !f.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
        None => std::future::pending().await,
    }
}

async fn abort_handle(handle: &mut dyn ContainerHandle, reason: CompassError) -> CompassError {
    match handle.kill().await {
        Ok(()) => reason,
        Err(kill_err) => CompassError::ContainerEngine(format!("{reason}; {kill_err}")),
    }
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
    detect_engine_with_candidates(
        cfg!(target_os = "linux"),
        &podman_candidates,
        &docker_candidates,
    )
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
        "no active Podman or Docker engine reachable; start rootless Podman or Docker Desktop"
            .into(),
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
            EngineKind::Kubernetes => {
                return Err(CompassError::ContainerEngine(
                    "Kubernetes backend does not use local OCI CLI arguments".into(),
                ));
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
pub fn prepare_stage_permissions(
    stage_root: &Path,
    in_dir: &Path,
    out_dir: &Path,
) -> CompassResult<()> {
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

/// Hardened local Podman/Docker implementation of [`ContainerRunner`] (`W1`).
pub type LocalOciRunner = OciContainerRunner;

/// Hardened OCI container runner executing real Podman/Docker containers with live log streaming.
#[derive(Debug, Clone)]
pub struct OciContainerRunner {
    engine: DetectedEngine,
    worker_cache: Option<Arc<worker_cache::WorkerCache>>,
    workspace_scope: Option<String>,
    active_containers: Arc<std::sync::Mutex<HashMap<RunId, String>>>,
}

impl OciContainerRunner {
    /// Detect and bind to an active host Podman/Docker engine.
    pub async fn detect() -> CompassResult<Self> {
        let engine = detect_container_engine().await?;
        Ok(Self::from_engine(engine))
    }

    /// Construct from a known `DetectedEngine`.
    pub fn from_engine(engine: DetectedEngine) -> Self {
        Self {
            engine,
            worker_cache: None,
            workspace_scope: None,
            active_containers: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
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
            EngineKind::Kubernetes => return false,
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
    pub async fn installed_execution_image(
        &self,
        image: &str,
        allow_dev: bool,
    ) -> CompassResult<OciImageRef> {
        let mut pinned = OciImageRef::parse(image, allow_dev)?;
        if pinned.sha256_digest.is_some() {
            if self.has_verified_local_digest(image).await {
                return Ok(pinned);
            }
        } else {
            let output = tokio::time::timeout(
                Duration::from_secs(4),
                Command::new(&self.engine.binary_path)
                    .args(["image", "inspect", "--format", "{{.Id}}", image])
                    .stdin(Stdio::null())
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .map_err(|_| CompassError::ContainerEngine("local image inspection timed out".into()))?
            .map_err(|e| CompassError::ContainerEngine(e.to_string()))?;
            let id = String::from_utf8_lossy(&output.stdout);
            let digest = id.trim().strip_prefix("sha256:").unwrap_or(id.trim());
            if output.status.success()
                && digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                pinned.raw = format!("sha256:{digest}");
                return Ok(pinned);
            }
        }
        Err(CompassError::NotFound(format!(
            "image is not installed locally: {image}"
        )))
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
        let stage_tag = format!(
            "local/bundled-{safe_slug}:stage-{safe_ver}-{}",
            &content_hash[..12]
        );
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
        if self
            .inspect_local_image_id(&pinned_local_tag)
            .await?
            .as_deref()
            != Some(digest.as_str())
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
        let actual = self
            .inspect_local_image_id(&parsed.raw)
            .await?
            .ok_or_else(|| {
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

    /// Enable the hardened file mailbox for a run with a provider query session.
    pub async fn execute_with_queries(
        &self,
        spec: &ContainerRunSpec,
        log_tx: Option<broadcast::Sender<RunLogEvent>>,
        revoked: Option<&AtomicBool>,
        responder: Option<Arc<dyn LiveQueryResponder>>,
    ) -> CompassResult<ContainerExecutionReport> {
        self.execute_with_start_notification(spec, log_tx, revoked, responder, None)
            .await
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
    /// and `/out` regular files (`SEC-28`), delegating to the shared [`supervise`] loop.
    pub async fn execute_with_limits(
        &self,
        spec: &ContainerRunSpec,
        log_tx: Option<broadcast::Sender<RunLogEvent>>,
        revoked: Option<&AtomicBool>,
        responder: Option<Arc<dyn LiveQueryResponder>>,
        started_tx: Option<tokio::sync::oneshot::Sender<()>>,
        max_output_bytes: Option<u64>,
    ) -> CompassResult<ContainerExecutionReport> {
        supervise(
            self,
            spec,
            SuperviseOptions {
                log_tx,
                revoked,
                live_query: responder,
                started_tx,
                max_output_bytes,
                ..SuperviseOptions::default()
            },
        )
        .await
    }
}

struct LocalContainerHandle {
    runner: OciContainerRunner,
    run_id: RunId,
    container_name: String,
    child: tokio::process::Child,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
}

impl Drop for LocalContainerHandle {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.runner.active_containers.lock()
            && guard.get(&self.run_id).map(String::as_str) == Some(self.container_name.as_str())
        {
            guard.remove(&self.run_id);
        }
    }
}

#[async_trait::async_trait]
impl ContainerHandle for LocalContainerHandle {
    fn container_name(&self) -> &str {
        &self.container_name
    }

    fn take_stdout(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>> {
        self.stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn AsyncRead + Send + Unpin>)
    }

    fn take_stderr(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>> {
        self.stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn AsyncRead + Send + Unpin>)
    }

    async fn wait(&mut self) -> CompassResult<i32> {
        let status = self.child.wait().await.map_err(|e| {
            CompassError::ContainerEngine(format!(
                "wait on container {} failed: {e}",
                self.container_name
            ))
        })?;
        Ok(status.code().unwrap_or(-1))
    }

    async fn kill(&mut self) -> CompassResult<()> {
        self.runner
            .abort_client_and_container(&mut self.child, &self.container_name)
            .await
    }
}

#[async_trait::async_trait]
impl ContainerRunner for OciContainerRunner {
    fn engine_kind(&self) -> EngineKind {
        self.engine.kind
    }

    fn engine_version(&self) -> String {
        self.engine.server_version.clone()
    }

    fn allows_local_bridge_egress(&self) -> bool {
        self.workspace_scope.is_some()
    }

    async fn prepare_image(
        &self,
        spec: &ContainerRunSpec,
    ) -> CompassResult<Option<Box<dyn Any + Send + Sync>>> {
        if spec.image.sha256_digest.is_some() {
            let lease = self.prepare_for_execution(&spec.image.raw).await?;
            Ok(lease.map(|l| Box::new(l) as Box<dyn Any + Send + Sync>))
        } else {
            Ok(None)
        }
    }

    async fn spawn(&self, spec: &ContainerRunSpec) -> CompassResult<Box<dyn ContainerHandle>> {
        let container_name = self.container_name(&spec.run_id);
        let mut args = build_hardened_oci_args_with_local_allowance(
            &self.engine,
            &container_name,
            spec,
            self.workspace_scope.is_some(),
        )?;
        self.label_native_job(&mut args);
        if spec.image.sha256_digest.is_some()
            || self.worker_cache.is_some()
            || spec.image.raw.starts_with("sha256:")
            || spec.image.repository.starts_with("local/bundled-")
        {
            // A development tag may use an already installed image. Only the
            // digest admission path can fetch images into a worker-owned engine.
            args.insert(1, "--pull=never".into());
        }

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

        if let Ok(mut guard) = self.active_containers.lock() {
            guard.insert(spec.run_id.clone(), container_name.clone());
        }

        Ok(Box::new(LocalContainerHandle {
            runner: self.clone(),
            run_id: spec.run_id.clone(),
            container_name,
            child,
            stdout: Some(stdout),
            stderr: Some(stderr),
        }))
    }

    async fn cancel(&self, run_id: &RunId) -> CompassResult<()> {
        let target = {
            let guard = self.active_containers.lock().ok();
            guard.and_then(|g| g.get(run_id).cloned())
        }
        .or_else(|| self.workspace_scope.as_ref().map(|_| self.container_name(run_id)));
        match target {
            Some(name) => self.force_remove_container(&name).await,
            None => Ok(()),
        }
    }
}

/// Measure regular-file bytes, total entry count, and filesystem free bytes under `root`
/// without following symlinks (`W1`, `W3`).
pub fn measure_out_dir_stats(root: &Path) -> OutDirStats {
    fn walk(dir: &Path, stats: &mut OutDirStats) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            stats.entry_count = stats.entry_count.saturating_add(1);
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_file() {
                stats.regular_bytes = stats.regular_bytes.saturating_add(meta.len());
            } else if meta.is_dir() {
                walk(&path, stats);
            }
        }
    }
    let mut stats = OutDirStats {
        regular_bytes: 0,
        entry_count: 0,
        free_bytes: filesystem_free_bytes(root),
    };
    walk(root, &mut stats);
    stats
}

async fn drain_optional_stream(
    stream: Option<Box<dyn AsyncRead + Send + Unpin>>,
    stream_name: &'static str,
    run_id: &RunId,
    log_tx: Option<&broadcast::Sender<RunLogEvent>>,
    max_output_bytes: Option<u64>,
    shared_quota_bytes: Option<&AtomicU64>,
) -> CompassResult<(Vec<String>, u64)> {
    match stream {
        Some(s) => {
            drain_stream_bounded(
                s,
                stream_name,
                run_id,
                log_tx,
                max_output_bytes,
                shared_quota_bytes,
            )
            .await
        }
        None => Ok((Vec::new(), 0)),
    }
}

async fn drain_stream_bounded<R: AsyncRead + Unpin>(
    mut stream: R,
    stream_name: &'static str,
    run_id: &RunId,
    log_tx: Option<&broadcast::Sender<RunLogEvent>>,
    max_output_bytes: Option<u64>,
    shared_quota_bytes: Option<&AtomicU64>,
) -> CompassResult<(Vec<String>, u64)> {
    const MAX_RETAINED_LINES: usize = 2_000;
    const MAX_LINE_BYTES: usize = 8_192;

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
            Err(_) => break,
            Ok(n) => {
                // SEC-28: Account EVERY raw byte on stdout/stderr against `max_output_bytes`
                // immediately mid-stream before retaining lines.
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
                        emit_line(
                            &current_line[..current_line.len().min(MAX_LINE_BYTES)],
                            &mut lines,
                        );
                        total_bytes = total_bytes.saturating_add(line_byte_count.saturating_add(1));
                        current_line.clear();
                        line_byte_count = 0;
                    } else {
                        line_byte_count = line_byte_count.saturating_add(1);
                        if current_line.len() < MAX_LINE_BYTES {
                            current_line.push(b);
                        }
                    }
                }
            }
        }
    }
    if !current_line.is_empty() {
        emit_line(
            &current_line[..current_line.len().min(MAX_LINE_BYTES)],
            &mut lines,
        );
        total_bytes = total_bytes.saturating_add(line_byte_count);
    }
    Ok((lines, total_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sec_28_unconditional_log_driver_none_and_max_output_bytes_enforcement() {
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
        let flood_line = b"stdout-flood-payload-payload-payload-payload-payload\n";
        let mut flood = Vec::new();
        for _ in 0..20 {
            flood.extend_from_slice(flood_line);
        }

        let (lines, counted) =
            drain_stream_bounded(&flood[..], "stderr", &run_id, None, None, None)
                .await
                .expect("drain without cap counts all bytes");
        assert_eq!(lines.len(), 20);
        assert_eq!(counted, flood.len() as u64);

        let err = drain_stream_bounded(&flood[..], "stderr", &run_id, None, Some(256), None)
            .await
            .expect_err("SEC-28 must reject stream flood over max_output_bytes");
        assert!(err.to_string().contains("OUTPUT_QUOTA_EXCEEDED"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn revocation_and_deadline_cancel_inflight_mailbox_queries_and_remove_container() {
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
        let in_dir = tmp.path().join("in");
        let out_dir = tmp.path().join("out");
        prepare_stage_permissions(tmp.path(), &in_dir, &out_dir).expect("stage perms");
        let binary_path = tmp.path().join("fake-docker");
        let removal = tmp.path().join("removed");
        std::fs::write(
            &binary_path,
            format!(
                r#"#!/bin/sh
case "$1" in
  run)
    for arg; do
      case "$arg" in
        --network=none) offline=1 ;;
        --log-driver=none) no_logs=1 ;;
        --tty|-t|--interactive|-i) exit 43 ;;
      esac
    done
    [ "$offline" = 1 ] && [ "$no_logs" = 1 ] || exit 44
    req_dir="{req_dir}"
    printf '{{"kind":"query","request_id":"q1","query":"demo","params":{{}}}}' > "$req_dir/.tmp-q1.tmp"
    mv "$req_dir/.tmp-q1.tmp" "$req_dir/q1.json"
    exec sleep 30
    ;;
  rm) printf '%s\n' "$3" > "${{0%/*}}/removed" ;;
  *) exit 1 ;;
esac
"#,
                req_dir = out_dir.join(MAILBOX_REQUESTS_REL).display()
            ),
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
            in_dir,
            out_dir,
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
                tokio::time::timeout(Duration::from_secs(3), responder.called.notified())
                    .await
                    .expect("module requests query via file mailbox");
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
            logs.contains("WARN")
                && logs.to_ascii_lowercase().contains("podman")
                && logs.to_ascii_lowercase().contains("docker"),
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
