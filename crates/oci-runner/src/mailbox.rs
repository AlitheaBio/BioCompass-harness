//! Hardened host-container file mailbox for live queries (`W2`) and brokered child runs (`W7`).
//!
//! Replaces the legacy stdin/stdout `BIOCOMPASS_QUERY_V1` pipe with atomic same-directory
//! rename files across the existing `/out` (`rw`) and `/in` (`ro` in container, `rw` on host)
//! staging directories so both local Docker/Podman containers and Kubernetes Run Pods share
//! one identical, zero-privilege IPC contract:
//!
//! - Container writes temp file (`.tmp-<id>.tmp`) inside `/out/.bc/requests/` and atomically
//!   renames it to `/out/.bc/requests/<id>.json`.
//! - Worker polls on the 50 ms supervision tick, opens requests strictly through a verified
//!   directory file descriptor (`openat` with `O_NOFOLLOW | O_NONBLOCK`, `fstat` regular-file
//!   check, bounded read), consumes the request file, and atomically writes the reply into
//!   `/in/.bc/replies/<id>.json` (mode `0644`).
//! - Any transport/filesystem violation (symlink, FIFO, subdirectory, path traversal,
//!   oversize payload, call-limit breach, more than [`MAX_PENDING_REQUEST_ENTRIES`] entries,
//!   malformed JSON, or `.bc` directory tampering) returns a `MAILBOX_VIOLATION` error so
//!   [`crate::supervise`] kills the container immediately. A re-used request ID is answered
//!   with a `DUPLICATE_REQUEST_ID` error reply and counts against the call limit.

use biocompass_core_traits::{
    CompassError, CompassResult, PluginId, RunId, RunInput, RunStatus, canonicalize_run_inputs,
};
use biocompass_plugin_spec::ChildModules;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

/// Relative directory under `/in` and `/out` reserved for BioCompass runtime IPC.
pub const MAILBOX_ROOT_REL: &str = ".bc";
/// Relative directory under `/out` where the module atomically renames `<request_id>.json`.
pub const MAILBOX_REQUESTS_REL: &str = ".bc/requests";
/// Relative directory under `/in` where the worker atomically writes `<request_id>.json`.
pub const MAILBOX_REPLIES_REL: &str = ".bc/replies";
/// Relative metadata file under `/in` advertising mailbox paths and child-run policy to the module.
pub const MAILBOX_CONFIG_REL: &str = ".bc/config.json";

/// Container-visible path to the request directory (`/out/.bc/requests`).
pub const CONTAINER_MAILBOX_REQUESTS_DIR: &str = "/out/.bc/requests";
/// Container-visible path to the reply directory (`/in/.bc/replies`).
pub const CONTAINER_MAILBOX_REPLIES_DIR: &str = "/in/.bc/replies";
/// Container-visible path where upstream and child output artifacts are staged (`/in/artifacts`).
pub const CONTAINER_ARTIFACTS_DIR: &str = "/in/artifacts";

/// Maximum serialized request payload for a `"query"` mailbox request (8 KiB).
pub const MAX_LIVE_QUERY_REQUEST_BYTES: usize = 8_192;
/// Maximum serialized request payload for any mailbox request file (256 KiB).
pub const MAX_MAILBOX_REQUEST_BYTES: usize = 256 * 1024;
/// Maximum serialized reply payload written to `/in/.bc/replies/<id>.json` (1 MiB).
pub const MAX_MAILBOX_REPLY_BYTES: usize = 1_048_576;
/// Default cap on total mailbox requests per run.
pub const DEFAULT_MAX_MAILBOX_CALLS: usize = 64;
/// Maximum number of directory entries tolerated in `/out/.bc/requests` at one poll.
/// A module flooding the directory (completed, temp or garbage files) is killed.
pub const MAX_PENDING_REQUEST_ENTRIES: usize = 128;
/// Maximum size of a child `result_json` staged as a file when it cannot be inlined (64 MiB).
pub const MAX_CHILD_RESULT_FILE_BYTES: usize = 64 * 1024 * 1024;
/// File name (inside the child's artifact alias directory) holding an oversize child `result_json`.
pub const CHILD_RESULT_FILE_NAME: &str = ".bc-result.json";
/// Error code written in a reply when a module re-uses a request ID.
pub const ERR_DUPLICATE_REQUEST_ID: &str = "DUPLICATE_REQUEST_ID";

/// Handles a live-query JSON frame (`{ request_id, query, params }`) for an air-gapped run.
#[async_trait::async_trait]
pub trait LiveQueryResponder: Send + Sync {
    async fn call(&self, frame: &[u8]) -> CompassResult<Vec<u8>>;
}

fn default_empty_object() -> Value {
    json!({})
}

/// Container-submitted `W7` `"run_module"` mailbox request parsed from `/out/.bc/requests/<id>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildModuleRequest {
    pub request_id: String,
    pub plugin_id: PluginId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer_id: Option<String>,
    #[serde(default = "default_empty_object")]
    pub params: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<RunInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_sec: Option<u64>,
}

/// One staged output artifact entry surfaced in `/in/.bc/replies/<id>.json` for a child run (`W7`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildRunStagedArtifact {
    pub name: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub media_type: String,
    #[serde(default)]
    pub staged_path: Option<String>,
}

/// Structured reply written atomically to `/in/.bc/replies/<id>.json` for a `W7` `"run_module"` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChildRunReply {
    pub request_id: String,
    pub ok: bool,
    #[serde(default)]
    pub status: Option<RunStatus>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub receipt_hash: Option<String>,
    #[serde(default)]
    pub result_json: Option<Value>,
    /// Container path of a JSON file holding `result_json` when it is too large to inline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_path: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<ChildRunStagedArtifact>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl ChildRunReply {
    /// Construct a structured rejection/failure reply for a `"run_module"` request.
    pub fn error(
        request_id: impl Into<String>,
        error_code: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            ok: false,
            status: None,
            run_id: None,
            receipt_hash: None,
            result_json: None,
            result_path: None,
            artifacts: Vec::new(),
            error_code: Some(error_code.into()),
            error: Some(error.into()),
        }
    }
}

/// Host-side execution context passed to [`ChildRunBroker::run_child`].
#[derive(Debug, Clone)]
pub struct ChildRunBrokerContext {
    pub parent_run_id: RunId,
    /// Host path to the parent's `/in` stage directory (for staging child artifacts under `artifacts/`).
    pub parent_in_dir: PathBuf,
    /// Remaining parent `max_child_wait_sec` allowance at the moment this child request was accepted.
    pub remaining_child_wait_sec: u64,
    /// Remaining bytes of child artifacts the parent may still stage under `/in/artifacts`
    /// (bounded by the parent's signed `max_input_bytes`); `None` means unbounded. Brokers must
    /// refuse (`INPUT_LIMIT_EXCEEDED`) a child whose artifacts exceed it before fetching them.
    pub remaining_child_artifact_bytes: Option<u64>,
}

/// Async broker callback invoked by the file mailbox when a running parent module submits
/// an allow-listed `"run_module"` request (`W7`).
#[async_trait::async_trait]
pub trait ChildRunBroker: Send + Sync {
    async fn run_child(
        &self,
        ctx: &ChildRunBrokerContext,
        request: ChildModuleRequest,
    ) -> CompassResult<ChildRunReply>;
}

/// Parsed live-query request forwarded unchanged to [`LiveQueryResponder::call`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveQueryMailboxRequest {
    pub request_id: String,
    pub query: String,
    pub params: Value,
}

/// Decoded request read from `/out/.bc/requests/<request_id>.json`.
#[derive(Debug, Clone, PartialEq)]
pub enum MailboxRequest {
    LiveQuery(LiveQueryMailboxRequest),
    ChildModule(ChildModuleRequest),
}

impl MailboxRequest {
    pub fn request_id(&self) -> &str {
        match self {
            Self::LiveQuery(q) => &q.request_id,
            Self::ChildModule(c) => &c.request_id,
        }
    }
}

/// Configuration for [`FileMailbox`].
#[derive(Clone)]
pub struct MailboxConfig {
    pub run_id: RunId,
    pub in_dir: PathBuf,
    pub out_dir: PathBuf,
    pub live_query: Option<Arc<dyn LiveQueryResponder>>,
    pub child_broker: Option<Arc<dyn ChildRunBroker>>,
    pub allowed_child_modules: ChildModules,
    pub max_child_wait_sec: Option<u64>,
    /// Signed container runtime limit (seconds) advertised to the module in `config.json`.
    pub max_runtime_sec: Option<u64>,
    pub max_calls: usize,
}

impl std::fmt::Debug for MailboxConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailboxConfig")
            .field("run_id", &self.run_id)
            .field("in_dir", &self.in_dir)
            .field("out_dir", &self.out_dir)
            .field("has_live_query", &self.live_query.is_some())
            .field("has_child_broker", &self.child_broker.is_some())
            .field("allowed_child_modules", &self.allowed_child_modules)
            .field("max_child_wait_sec", &self.max_child_wait_sec)
            .field("max_runtime_sec", &self.max_runtime_sec)
            .field("max_calls", &self.max_calls)
            .finish()
    }
}

/// Deterministic artifact staging alias for child run outputs under `/in/artifacts/<alias>/<name>`.
pub fn child_artifact_alias(request_id: &str) -> String {
    format!("child-{request_id}")
}

/// Validate a mailbox `request_id` (`1..=64` ASCII chars, starts with `[a-zA-Z0-9]`,
/// remaining `[a-zA-Z0-9._-]`, never containing `..`).
pub fn is_valid_request_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 64 || id.contains("..") {
        return false;
    }
    let bytes = id.as_bytes();
    bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn is_valid_relative_artifact_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains(['\\', '\0'])
        && !name.split('/').any(|seg| matches!(seg, "" | "." | ".."))
}

/// Stage verified bytes for a child-run artifact into `<in_dir>/artifacts/<alias>/<name>`
/// atomically (mode `0644`, parent dirs `0755`) and return the container-visible path
/// `/in/artifacts/<alias>/<name>`.
pub fn stage_child_artifact_bytes(
    in_dir: &Path,
    alias: &str,
    name: &str,
    bytes: &[u8],
) -> CompassResult<String> {
    if !is_valid_request_id(alias) {
        return Err(CompassError::Validation(format!(
            "invalid child artifact staging alias '{alias}'"
        )));
    }
    if !is_valid_relative_artifact_name(name) {
        return Err(CompassError::Validation(format!(
            "invalid child artifact relative name '{name}'"
        )));
    }
    let artifacts_root = in_dir.join("artifacts");
    let alias_dir = artifacts_root.join(alias);
    let dest = alias_dir.join(name);
    let parent = dest
        .parent()
        .ok_or_else(|| CompassError::Storage("artifact destination has no parent".into()))?;
    create_dir_all_with_mode(in_dir, &artifacts_root, 0o755)?;
    create_dir_all_with_mode(&artifacts_root, parent, 0o755)?;
    atomic_write_readable_file(&dest, bytes)?;
    Ok(format!("{CONTAINER_ARTIFACTS_DIR}/{alias}/{name}"))
}

/// Create `<out_dir>/.bc/requests` (`0777`), `<in_dir>/.bc/replies` (`0755`), and write
/// `<in_dir>/.bc/config.json` (`0644`) before launching a container with mailbox IPC enabled.
pub fn prepare_mailbox_dirs(
    in_dir: &Path,
    out_dir: &Path,
    allowed_child_modules: &ChildModules,
    max_child_wait_sec: Option<u64>,
    max_runtime_sec: Option<u64>,
) -> CompassResult<()> {
    let out_bc = out_dir.join(MAILBOX_ROOT_REL);
    let out_requests = out_dir.join(MAILBOX_REQUESTS_REL);
    let in_bc = in_dir.join(MAILBOX_ROOT_REL);
    let in_replies = in_dir.join(MAILBOX_REPLIES_REL);

    std::fs::create_dir_all(&out_requests)
        .map_err(|e| CompassError::Storage(format!("create /out/.bc/requests: {e}")))?;
    std::fs::create_dir_all(&in_replies)
        .map_err(|e| CompassError::Storage(format!("create /in/.bc/replies: {e}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&out_bc, std::fs::Permissions::from_mode(0o777))
            .map_err(|e| CompassError::Storage(format!("chmod 0777 /out/.bc: {e}")))?;
        std::fs::set_permissions(&out_requests, std::fs::Permissions::from_mode(0o777))
            .map_err(|e| CompassError::Storage(format!("chmod 0777 /out/.bc/requests: {e}")))?;
        std::fs::set_permissions(&in_bc, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| CompassError::Storage(format!("chmod 0755 /in/.bc: {e}")))?;
        std::fs::set_permissions(&in_replies, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| CompassError::Storage(format!("chmod 0755 /in/.bc/replies: {e}")))?;
    }

    let config_doc = json!({
        "version": 1,
        "requests_dir": CONTAINER_MAILBOX_REQUESTS_DIR,
        "replies_dir": CONTAINER_MAILBOX_REPLIES_DIR,
        "artifacts_dir": CONTAINER_ARTIFACTS_DIR,
        "mailbox_enabled": true,
        "child_modules": allowed_child_modules.to_json(),
        "max_child_wait_sec": max_child_wait_sec,
        "max_runtime_sec": max_runtime_sec,
    });
    let config_bytes = serde_json::to_vec(&config_doc)
        .map_err(|e| CompassError::Internal(format!("encode mailbox config: {e}")))?;
    atomic_write_readable_file(&in_dir.join(MAILBOX_CONFIG_REL), &config_bytes)?;
    Ok(())
}

fn create_dir_all_with_mode(base: &Path, target: &Path, mode: u32) -> CompassResult<()> {
    std::fs::create_dir_all(target)
        .map_err(|e| CompassError::Storage(format!("create {}: {e}", target.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut cur = target;
        while cur != base {
            std::fs::set_permissions(cur, std::fs::Permissions::from_mode(mode)).map_err(|e| {
                CompassError::Storage(format!("chmod {:o} {}: {e}", mode, cur.display()))
            })?;
            match cur.parent() {
                Some(parent) => cur = parent,
                None => break,
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (base, mode);
    Ok(())
}

fn atomic_write_readable_file(dest: &Path, bytes: &[u8]) -> CompassResult<()> {
    let parent = dest
        .parent()
        .ok_or_else(|| CompassError::Storage(format!("missing parent for {}", dest.display())))?;
    let file_name = dest
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| CompassError::Storage(format!("invalid filename {}", dest.display())))?;
    let tmp_path = parent.join(format!(".tmp-{file_name}-{}", Uuid::new_v4().simple()));
    std::fs::write(&tmp_path, bytes)
        .map_err(|e| CompassError::Storage(format!("write temp {}: {e}", tmp_path.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o644))
        {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(CompassError::Storage(format!(
                "chmod 0644 {}: {e}",
                tmp_path.display()
            )));
        }
    }
    if let Err(e) = std::fs::rename(&tmp_path, dest) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(CompassError::Storage(format!(
            "atomic rename {} -> {}: {e}",
            tmp_path.display(),
            dest.display()
        )));
    }
    Ok(())
}

fn violation(detail: impl std::fmt::Display) -> CompassError {
    CompassError::ContainerEngine(format!("MAILBOX_VIOLATION: {detail}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SeenRequest {
    unlinked: bool,
    dev: u64,
    ino: u64,
    len: u64,
    sha256: [u8; 32],
}

/// Host-side mailbox reader/writer pinned to the verified `/out/.bc/requests` directory fd.
pub struct FileMailbox {
    cfg: MailboxConfig,
    replies_dir: PathBuf,
    #[cfg(unix)]
    out_dir_fd: rustix::fd::OwnedFd,
    #[cfg(unix)]
    requests_dir_fd: rustix::fd::OwnedFd,
    #[cfg(unix)]
    requests_dev_ino: (u64, u64),
    #[cfg(not(unix))]
    requests_dir: PathBuf,
    seen_requests: HashMap<String, SeenRequest>,
    calls_used: usize,
    child_receipt_hashes: BTreeSet<String>,
}

impl FileMailbox {
    /// Prepare `/out/.bc/requests` & `/in/.bc/replies` and open pinned directory descriptors.
    pub fn open(cfg: MailboxConfig) -> CompassResult<Self> {
        prepare_mailbox_dirs(
            &cfg.in_dir,
            &cfg.out_dir,
            &cfg.allowed_child_modules,
            cfg.max_child_wait_sec,
            cfg.max_runtime_sec,
        )?;
        let replies_dir = cfg.in_dir.join(MAILBOX_REPLIES_REL);

        #[cfg(unix)]
        {
            use rustix::fs::{CWD, Mode, OFlags, fstat, openat};
            let out_dir_fd = openat(
                CWD,
                &cfg.out_dir,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| CompassError::Storage(format!("open /out dir fd: {e}")))?;
            let bc_fd = openat(
                &out_dir_fd,
                MAILBOX_ROOT_REL,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| CompassError::Storage(format!("open /out/.bc dir fd: {e}")))?;
            let requests_dir_fd = openat(
                &bc_fd,
                "requests",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| CompassError::Storage(format!("open /out/.bc/requests dir fd: {e}")))?;
            let stat = fstat(&requests_dir_fd)
                .map_err(|e| CompassError::Storage(format!("fstat /out/.bc/requests: {e}")))?;
            let requests_dev_ino = (stat.st_dev as u64, stat.st_ino as u64);
            Ok(Self {
                cfg,
                replies_dir,
                out_dir_fd,
                requests_dir_fd,
                requests_dev_ino,
                seen_requests: HashMap::new(),
                calls_used: 0,
                child_receipt_hashes: BTreeSet::new(),
            })
        }

        #[cfg(not(unix))]
        {
            let requests_dir = cfg.out_dir.join(MAILBOX_REQUESTS_REL);
            Ok(Self {
                cfg,
                replies_dir,
                requests_dir,
                seen_requests: HashMap::new(),
                calls_used: 0,
                child_receipt_hashes: BTreeSet::new(),
            })
        }
    }

    pub fn config(&self) -> &MailboxConfig {
        &self.cfg
    }

    pub fn record_child_receipt_hash(&mut self, hash: String) {
        self.child_receipt_hashes.insert(hash);
    }

    pub fn child_receipt_hashes(&self) -> Vec<String> {
        self.child_receipt_hashes.iter().cloned().collect()
    }

    /// Re-verify that `/out/.bc` and `/out/.bc/requests` were not unlinked or replaced with symlinks.
    pub fn verify_dir_integrity(&self) -> CompassResult<()> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, fstat, openat};
            let bc_fd = openat(
                &self.out_dir_fd,
                MAILBOX_ROOT_REL,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| violation(format!("/out/.bc directory tampered or missing: {e}")))?;
            let req_fd = openat(
                &bc_fd,
                "requests",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| {
                violation(format!("/out/.bc/requests directory tampered or missing: {e}"))
            })?;
            let stat = fstat(&req_fd)
                .map_err(|e| violation(format!("fstat /out/.bc/requests failed: {e}")))?;
            if (stat.st_dev as u64, stat.st_ino as u64) != self.requests_dev_ino {
                return Err(violation(
                    "/out/.bc/requests inode changed during execution (possible directory swap)",
                ));
            }
        }
        Ok(())
    }

    /// Scan `/out/.bc/requests` once (non-blocking) and return the next valid request, if any.
    /// Rejects symlinks, FIFOs, subdirectories, path traversal, oversize payloads, duplicate IDs,
    /// call-limit breaches, and malformed JSON with `MAILBOX_VIOLATION`.
    pub fn poll_next_request(&mut self) -> CompassResult<Option<MailboxRequest>> {
        self.verify_dir_integrity()?;
        let entries = self.list_request_entries()?;
        if entries.len() > MAX_PENDING_REQUEST_ENTRIES {
            return Err(violation(format!(
                "/out/.bc/requests holds {} entries (limit {MAX_PENDING_REQUEST_ENTRIES})",
                entries.len()
            )));
        }
        for name in entries {
            if is_temp_entry_name(&name) {
                self.validate_temp_entry(&name)?;
                continue;
            }
            let Some(stem) = name.strip_suffix(".json") else {
                return Err(violation(format!(
                    "unexpected non-JSON entry '{name}' in /out/.bc/requests"
                )));
            };
            if !is_valid_request_id(stem) {
                return Err(violation(format!(
                    "invalid request filename '{name}' in /out/.bc/requests"
                )));
            }
            let Some((bytes, dev, ino)) = self.read_regular_request_file(&name)? else {
                // File vanished concurrently before open; skip.
                continue;
            };
            if bytes.is_empty() {
                return Err(violation(format!(
                    "empty mailbox request file '{name}' in /out/.bc/requests"
                )));
            }
            let sha256: [u8; 32] = Sha256::digest(&bytes).into();
            let len = bytes.len() as u64;
            if let Some(prev) = self.seen_requests.get(stem) {
                if !prev.unlinked
                    && prev.dev == dev
                    && prev.ino == ino
                    && prev.len == len
                    && prev.sha256 == sha256
                {
                    // Same file we already consumed but could not unlink.
                    continue;
                }
                // A re-used request ID gets a structured error reply instead of killing the run.
                // It still counts against the call limit so a module cannot spin on it.
                if self.calls_used >= self.cfg.max_calls {
                    return Err(violation(format!(
                        "mailbox call limit ({}) exceeded",
                        self.cfg.max_calls
                    )));
                }
                self.calls_used += 1;
                if !self.unlink_request_entry(&name) {
                    return Err(violation(format!(
                        "cannot consume duplicate mailbox request '{name}'"
                    )));
                }
                self.write_child_reply(&ChildRunReply::error(
                    stem,
                    ERR_DUPLICATE_REQUEST_ID,
                    format!("request_id '{stem}' was already used in this run"),
                ))?;
                continue;
            }
            if self.calls_used >= self.cfg.max_calls {
                return Err(violation(format!(
                    "mailbox call limit ({}) exceeded",
                    self.cfg.max_calls
                )));
            }
            let unlinked = self.unlink_request_entry(&name);
            self.seen_requests.insert(
                stem.to_owned(),
                SeenRequest {
                    unlinked,
                    dev,
                    ino,
                    len,
                    sha256,
                },
            );
            self.calls_used += 1;

            let parsed = parse_mailbox_request(stem, &bytes)?;
            return Ok(Some(parsed));
        }
        Ok(None)
    }

    /// Atomically write raw JSON reply bytes to `/in/.bc/replies/<request_id>.json`.
    pub fn write_reply_bytes(&self, request_id: &str, bytes: &[u8]) -> CompassResult<()> {
        if !is_valid_request_id(request_id) {
            return Err(CompassError::Validation(format!(
                "invalid reply request_id '{request_id}'"
            )));
        }
        if bytes.is_empty() || bytes.len() > MAX_MAILBOX_REPLY_BYTES {
            return Err(CompassError::ContainerEngine(format!(
                "mailbox reply for '{request_id}' out of bounds ({} bytes, max {MAX_MAILBOX_REPLY_BYTES})",
                bytes.len()
            )));
        }
        let _: Value = serde_json::from_slice(bytes).map_err(|e| {
            CompassError::ContainerEngine(format!("mailbox reply for '{request_id}' is not valid JSON: {e}"))
        })?;
        atomic_write_readable_file(&self.replies_dir.join(format!("{request_id}.json")), bytes)
    }

    /// Atomically write a structured [`ChildRunReply`] to `/in/.bc/replies/<request_id>.json`.
    /// A `result_json` too large to inline (reply cap 1 MiB) is staged as
    /// `/in/artifacts/child-<request_id>/.bc-result.json` and referenced by `result_path`, so a
    /// large child result never kills the parent. Results above [`MAX_CHILD_RESULT_FILE_BYTES`]
    /// are replaced by a structured `CHILD_RESULT_TOO_LARGE` error reply.
    pub fn write_child_reply(&self, reply: &ChildRunReply) -> CompassResult<()> {
        let bytes = serde_json::to_vec(reply)
            .map_err(|e| CompassError::Internal(format!("encode child run reply: {e}")))?;
        if bytes.len() <= MAX_MAILBOX_REPLY_BYTES {
            return self.write_reply_bytes(&reply.request_id, &bytes);
        }
        let mut spilled = reply.clone();
        let result = spilled.result_json.take();
        let reply_bytes = match result {
            Some(result) => {
                let result_bytes = serde_json::to_vec(&result)
                    .map_err(|e| CompassError::Internal(format!("encode child result: {e}")))?;
                if result_bytes.len() > MAX_CHILD_RESULT_FILE_BYTES {
                    serde_json::to_vec(&ChildRunReply {
                        status: reply.status,
                        run_id: reply.run_id.clone(),
                        receipt_hash: reply.receipt_hash.clone(),
                        ..ChildRunReply::error(
                            reply.request_id.clone(),
                            "CHILD_RESULT_TOO_LARGE",
                            format!(
                                "child result_json ({} bytes) exceeds {MAX_CHILD_RESULT_FILE_BYTES} bytes",
                                result_bytes.len()
                            ),
                        )
                    })
                } else {
                    spilled.result_path = Some(stage_child_artifact_bytes(
                        &self.cfg.in_dir,
                        &child_artifact_alias(&reply.request_id),
                        CHILD_RESULT_FILE_NAME,
                        &result_bytes,
                    )?);
                    serde_json::to_vec(&spilled)
                }
            }
            None => serde_json::to_vec(&spilled),
        }
        .map_err(|e| CompassError::Internal(format!("encode child run reply: {e}")))?;
        self.write_reply_bytes(&reply.request_id, &reply_bytes)
    }

    /// Check whether a `"run_module"` target is permitted by this module's `child_modules`
    /// (`"caller"` permits every module; the hub then authorises the initiating principal).
    pub fn is_child_module_allowed(&self, plugin_id: &PluginId) -> bool {
        self.cfg.allowed_child_modules.permits(plugin_id.as_str())
    }

    #[cfg(unix)]
    fn list_request_entries(&self) -> CompassResult<Vec<String>> {
        use rustix::fs::{Dir, Mode, OFlags, openat};
        // Open a fresh handle on the verified directory inode via `.` so `Dir` starts at offset 0.
        let dir_fd = openat(
            &self.requests_dir_fd,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| violation(format!("reopen /out/.bc/requests dir fd: {e}")))?;
        let mut dir = Dir::new(dir_fd)
            .map_err(|e| violation(format!("iterate /out/.bc/requests dir fd: {e}")))?;
        let mut names = Vec::new();
        for entry in dir.by_ref() {
            let entry = entry.map_err(|e| violation(format!("read /out/.bc/requests entry: {e}")))?;
            let raw_bytes = entry.file_name().to_bytes();
            if raw_bytes == b"." || raw_bytes == b".." {
                continue;
            }
            let name = std::str::from_utf8(raw_bytes)
                .map_err(|_| violation("non-UTF-8 entry name in /out/.bc/requests"))?;
            if name.contains('/') || name.contains('\0') {
                return Err(violation(format!(
                    "path traversal characters in entry '{name}'"
                )));
            }
            names.push(name.to_owned());
        }
        names.sort();
        Ok(names)
    }

    #[cfg(not(unix))]
    fn list_request_entries(&self) -> CompassResult<Vec<String>> {
        let mut names = Vec::new();
        let entries = std::fs::read_dir(&self.requests_dir)
            .map_err(|e| violation(format!("read /out/.bc/requests: {e}")))?;
        for entry in entries {
            let entry = entry.map_err(|e| violation(format!("read entry: {e}")))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| violation("non-UTF-8 entry name in /out/.bc/requests"))?;
            if name.contains(['/', '\\', '\0']) {
                return Err(violation(format!(
                    "path traversal characters in entry '{name}'"
                )));
            }
            names.push(name);
        }
        names.sort();
        Ok(names)
    }

    #[cfg(unix)]
    fn validate_temp_entry(&self, name: &str) -> CompassResult<()> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat};
        use rustix::io::Errno;
        match openat(
            &self.requests_dir_fd,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => {
                let stat = fstat(&fd)
                    .map_err(|e| violation(format!("fstat temp entry '{name}': {e}")))?;
                let ft = FileType::from_raw_mode(stat.st_mode);
                if ft != FileType::RegularFile {
                    return Err(violation(format!(
                        "non-regular temp entry '{name}' ({ft:?}) rejected in /out/.bc/requests"
                    )));
                }
                if stat.st_size < 0 || (stat.st_size as u64) > (MAX_MAILBOX_REQUEST_BYTES as u64) {
                    return Err(violation(format!(
                        "oversize temp entry '{name}' ({} bytes) exceeds limit ({MAX_MAILBOX_REQUEST_BYTES})",
                        stat.st_size
                    )));
                }
                Ok(())
            }
            Err(Errno::NOENT) => Ok(()), // Concurrently renamed into `<id>.json`.
            Err(Errno::LOOP) => Err(violation(format!(
                "symlink '{name}' rejected in /out/.bc/requests"
            ))),
            Err(e) => Err(violation(format!(
                "cannot safely inspect temp entry '{name}' in /out/.bc/requests: {e}"
            ))),
        }
    }

    #[cfg(not(unix))]
    fn validate_temp_entry(&self, name: &str) -> CompassResult<()> {
        let path = self.requests_dir.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) => {
                if meta.file_type().is_symlink() || !meta.is_file() {
                    return Err(violation(format!(
                        "non-regular temp entry '{name}' rejected in /out/.bc/requests"
                    )));
                }
                if meta.len() > MAX_MAILBOX_REQUEST_BYTES as u64 {
                    return Err(violation(format!(
                        "oversize temp entry '{name}' exceeds limit ({MAX_MAILBOX_REQUEST_BYTES})"
                    )));
                }
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(violation(format!("stat temp entry '{name}': {e}"))),
        }
    }

    #[cfg(unix)]
    fn read_regular_request_file(&self, name: &str) -> CompassResult<Option<(Vec<u8>, u64, u64)>> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat};
        use rustix::io::Errno;
        let fd = match openat(
            &self.requests_dir_fd,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(Errno::LOOP) => {
                return Err(violation(format!(
                    "symlink '{name}' rejected in /out/.bc/requests"
                )));
            }
            Err(e) => {
                return Err(violation(format!(
                    "openat '{name}' in /out/.bc/requests failed: {e}"
                )));
            }
        };
        let stat =
            fstat(&fd).map_err(|e| violation(format!("fstat '{name}' in /out/.bc/requests: {e}")))?;
        let ft = FileType::from_raw_mode(stat.st_mode);
        if ft != FileType::RegularFile {
            return Err(violation(format!(
                "non-regular file '{name}' ({ft:?}) rejected in /out/.bc/requests"
            )));
        }
        if stat.st_size < 0 || (stat.st_size as u64) > (MAX_MAILBOX_REQUEST_BYTES as u64) {
            return Err(violation(format!(
                "mailbox request '{name}' size ({} bytes) exceeds limit ({MAX_MAILBOX_REQUEST_BYTES})",
                stat.st_size
            )));
        }
        let dev = stat.st_dev as u64;
        let ino = stat.st_ino as u64;
        let file = std::fs::File::from(fd);
        let mut buf = Vec::with_capacity((stat.st_size as usize).min(MAX_MAILBOX_REQUEST_BYTES));
        file.take((MAX_MAILBOX_REQUEST_BYTES + 1) as u64)
            .read_to_end(&mut buf)
            .map_err(|e| violation(format!("read '{name}' in /out/.bc/requests: {e}")))?;
        if buf.len() > MAX_MAILBOX_REQUEST_BYTES {
            return Err(violation(format!(
                "mailbox request '{name}' exceeded {MAX_MAILBOX_REQUEST_BYTES} bytes during read"
            )));
        }
        Ok(Some((buf, dev, ino)))
    }

    #[cfg(not(unix))]
    fn read_regular_request_file(&self, name: &str) -> CompassResult<Option<(Vec<u8>, u64, u64)>> {
        let path = self.requests_dir.join(name);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(violation(format!("stat '{name}': {e}"))),
        };
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(violation(format!(
                "non-regular file '{name}' rejected in /out/.bc/requests"
            )));
        }
        if meta.len() > MAX_MAILBOX_REQUEST_BYTES as u64 {
            return Err(violation(format!(
                "mailbox request '{name}' exceeds limit ({MAX_MAILBOX_REQUEST_BYTES})"
            )));
        }
        let file = open_no_follow(&path).map_err(|e| violation(format!("open '{name}': {e}")))?;
        // Re-check the opened handle: a swap to a symlink/reparse point or non-file between the
        // `symlink_metadata` and the open is rejected (no `O_NOFOLLOW` on this platform).
        let opened = file
            .metadata()
            .map_err(|e| violation(format!("stat opened '{name}': {e}")))?;
        if !opened.is_file() || opened.file_type().is_symlink() || opened.len() != meta.len() {
            return Err(violation(format!(
                "mailbox request '{name}' changed type while opening"
            )));
        }
        let mut buf = Vec::new();
        file.take((MAX_MAILBOX_REQUEST_BYTES + 1) as u64)
            .read_to_end(&mut buf)
            .map_err(|e| violation(format!("read '{name}': {e}")))?;
        if buf.len() > MAX_MAILBOX_REQUEST_BYTES {
            return Err(violation(format!(
                "mailbox request '{name}' exceeded {MAX_MAILBOX_REQUEST_BYTES} bytes during read"
            )));
        }
        Ok(Some((buf, 0, 0)))
    }

    #[cfg(unix)]
    fn unlink_request_entry(&self, name: &str) -> bool {
        use rustix::fs::{AtFlags, unlinkat};
        unlinkat(&self.requests_dir_fd, name, AtFlags::empty()).is_ok()
    }

    #[cfg(not(unix))]
    fn unlink_request_entry(&self, name: &str) -> bool {
        std::fs::remove_file(self.requests_dir.join(name)).is_ok()
    }
}

/// Open without following a final symlink / reparse point on non-Unix hosts.
#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: open the link itself rather than its target.
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT
        if file.metadata()?.file_attributes() & 0x400 != 0 {
            return Err(std::io::Error::other("reparse point rejected"));
        }
    }
    Ok(file)
}

fn is_temp_entry_name(name: &str) -> bool {
    name.starts_with('.') || name.ends_with(".tmp")
}

/// Parse and validate one JSON request file against its filename stem (`expected_request_id`).
fn parse_mailbox_request(expected_request_id: &str, bytes: &[u8]) -> CompassResult<MailboxRequest> {
    let mut obj: Map<String, Value> = serde_json::from_slice(bytes)
        .map_err(|e| violation(format!("malformed JSON in '{expected_request_id}.json': {e}")))?;

    let kind = match obj.remove("kind") {
        None => "query".to_owned(),
        Some(Value::String(k)) => k,
        Some(_) => {
            return Err(violation(format!(
                "mailbox request '{expected_request_id}.json' has non-string 'kind'"
            )));
        }
    };

    match kind.as_str() {
        "query" => {
            if bytes.len() > MAX_LIVE_QUERY_REQUEST_BYTES {
                return Err(violation(format!(
                    "live query request '{expected_request_id}' exceeded {MAX_LIVE_QUERY_REQUEST_BYTES} bytes"
                )));
            }
            let req: LiveQueryMailboxRequest = serde_json::from_value(Value::Object(obj))
                .map_err(|e| {
                    violation(format!(
                        "invalid live query request '{expected_request_id}.json': {e}"
                    ))
                })?;
            if req.request_id != expected_request_id {
                return Err(violation(format!(
                    "mailbox request_id '{}' does not match filename '{expected_request_id}.json'",
                    req.request_id
                )));
            }
            Ok(MailboxRequest::LiveQuery(req))
        }
        "run_module" => {
            let mut req: ChildModuleRequest = serde_json::from_value(Value::Object(obj))
                .map_err(|e| {
                    violation(format!(
                        "invalid run_module request '{expected_request_id}.json': {e}"
                    ))
                })?;
            if req.request_id != expected_request_id {
                return Err(violation(format!(
                    "mailbox request_id '{}' does not match filename '{expected_request_id}.json'",
                    req.request_id
                )));
            }
            if !req.inputs.is_empty() {
                req.inputs = canonicalize_run_inputs(&req.inputs).map_err(|e| {
                    violation(format!(
                        "invalid inputs in run_module request '{expected_request_id}': {e}"
                    ))
                })?;
            }
            Ok(MailboxRequest::ChildModule(req))
        }
        other => Err(violation(format!(
            "unsupported mailbox request kind '{other}' in '{expected_request_id}.json'"
        ))),
    }
}

/// Total size in bytes of the filesystem containing `path` (best-effort via `statvfs` on Unix).
pub fn filesystem_total_bytes(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        rustix::fs::statvfs(path)
            .ok()
            .map(|st| st.f_blocks.saturating_mul(st.f_frsize))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Available free bytes on the filesystem containing `path` (best-effort via `statvfs` on Unix).
pub fn filesystem_free_bytes(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        rustix::fs::statvfs(path)
            .ok()
            .map(|st| st.f_bavail.saturating_mul(st.f_frsize))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}
