use biocompass_core_traits::{CompassError, CompassResult, RunId};
use sha2::{Digest, Sha256};
use std::fs::{File, TryLockError};
use std::path::Path;
use std::time::Duration;

fn open(stage_root: &Path, run_id: &RunId) -> CompassResult<File> {
    // The inode is shared by all native processes using this workspace. Keep it after
    // unlocking so a new contender cannot lock a replacement inode while a waiter holds the old one.
    let stage_root = stage_root.canonicalize().map_err(|e| {
        CompassError::Storage(format!("resolve stage root {}: {e}", stage_root.display()))
    })?;
    let dir = stage_root.with_added_extension("run-locks");
    if dir == stage_root {
        return Err(CompassError::Validation(
            "the filesystem root cannot be used as a run staging directory".into(),
        ));
    }
    std::fs::create_dir_all(&dir).map_err(|e| {
        CompassError::Storage(format!("create run lock directory {}: {e}", dir.display()))
    })?;
    let name = hex::encode(Sha256::digest(run_id.as_str().as_bytes()));
    let path = dir.join(name);
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| CompassError::Storage(format!("open run lock {}: {e}", path.display())))
}

fn try_lock(file: &File, run_id: &RunId) -> CompassResult<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(err)) => {
            Err(CompassError::Storage(format!("lock run '{run_id}': {err}")))
        }
    }
}

/// Recovery skips live execution. The OS releases the lock when the last file handle closes.
pub(super) fn try_acquire(stage_root: &Path, run_id: &RunId) -> CompassResult<Option<File>> {
    let file = open(stage_root, run_id)?;
    Ok(try_lock(&file, run_id)?.then_some(file))
}

/// Wait without occupying a Tokio worker or a blocking-pool thread during a long execution.
pub(super) async fn acquire(stage_root: &Path, run_id: &RunId) -> CompassResult<File> {
    let file = open(stage_root, run_id)?;
    while !try_lock(&file, run_id)? {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(file)
}
