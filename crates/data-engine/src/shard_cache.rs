use super::{
    CompassError, CompassResult, Connection, SealedSnapshot, open_parquet_only_duckdb,
    validate_sha256_hex,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, Metadata};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Debug)]
struct TableSelection {
    tables: BTreeSet<String>,
    shards: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    device: u64,
    inode: u64,
    len: u64,
    ctime: i64,
    ctime_nsec: i64,
}

impl FileStamp {
    #[cfg(unix)]
    fn new(metadata: &Metadata) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        Some(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            len: metadata.len(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        })
    }
    // A system without a reliable change timestamp rehashes each cached file.
    #[cfg(not(unix))]
    fn new(_: &Metadata) -> Option<Self> {
        None
    }
}

/// Verify-once content-addressed Parquet shard cache (`<root>/sha256/<64-hex>.parquet`, fixing `B12`).
#[derive(Debug, Clone)]
pub struct ShardCache {
    sha256_dir: PathBuf,
    // Fixed stripes bound the number of synchronization objects for arbitrary catalogs.
    fetch_locks: Arc<[Arc<tokio::sync::Mutex<()>>; 64]>,
    cold_fills: Arc<tokio::sync::Semaphore>,
    fingerprints: Arc<Mutex<BTreeMap<String, FileStamp>>>,
    selection: Option<Arc<TableSelection>>,
    verified_ingests: Arc<AtomicU64>,
    parquet_reader: Arc<Mutex<Option<Connection>>>,
}

impl ShardCache {
    pub fn open(cache_root: impl AsRef<Path>) -> CompassResult<Self> {
        let sha256_dir = cache_root.as_ref().join("sha256");
        std::fs::create_dir_all(&sha256_dir).map_err(|e| {
            CompassError::Storage(format!("create shard cache {}: {e}", sha256_dir.display()))
        })?;
        Ok(Self {
            sha256_dir,
            fetch_locks: Arc::new(std::array::from_fn(|_| {
                Arc::new(tokio::sync::Mutex::new(()))
            })),
            cold_fills: Arc::new(tokio::sync::Semaphore::new(4)),
            fingerprints: Arc::default(),
            selection: None,
            verified_ingests: Arc::new(AtomicU64::new(0)),
            parquet_reader: Arc::new(Mutex::new(None)),
        })
    }

    /// Share the initialized Parquet engine. Each request has its own SQL session.
    pub(super) fn read_connection(&self) -> CompassResult<Connection> {
        let mut reader = self
            .parquet_reader
            .lock()
            .map_err(|_| CompassError::Storage("Parquet reader lock poisoned".into()))?;
        let connection = match reader.as_ref() {
            Some(existing) => existing,
            None => reader.insert(open_parquet_only_duckdb()?),
        };
        connection
            .try_clone()
            .map_err(|e| CompassError::Storage(format!("open Parquet read session: {e}")))
    }

    pub const fn sha256_dir(&self) -> &PathBuf {
        &self.sha256_dir
    }

    /// Count of SHA-256 verification ingestions performed by this cache instance (for `B12` tests).
    pub fn verified_ingest_count(&self) -> u64 {
        self.verified_ingests.load(Ordering::Relaxed)
    }

    pub fn shard_path(&self, sha256: &str) -> CompassResult<PathBuf> {
        validate_sha256_hex(sha256)?;
        if self
            .selection
            .as_ref()
            .is_some_and(|selection| !selection.shards.contains(sha256))
        {
            return Err(CompassError::AccessDenied(
                "shard is outside the selected tables".into(),
            ));
        }
        Ok(self.sha256_dir.join(format!("{sha256}.parquet")))
    }

    pub(super) fn restrict_to(
        &mut self,
        tables: &BTreeSet<String>,
        seal: &SealedSnapshot,
    ) -> CompassResult<()> {
        let mut shards = BTreeSet::new();
        for name in tables {
            let table = seal
                .tables
                .iter()
                .find(|t| t.schema.name == *name)
                .ok_or_else(|| {
                    CompassError::NotFound(format!("selected table '{name}' is missing"))
                })?;
            self.check_table(name)?;
            for pair in &table.files {
                for digest in std::iter::once(&pair.data_sha256).chain(&pair.delete_sha256) {
                    self.shard_path(digest)?;
                    shards.insert(digest.clone());
                }
            }
        }
        self.selection = Some(Arc::new(TableSelection {
            tables: tables.clone(),
            shards,
        }));
        Ok(())
    }

    pub(super) fn check_table(&self, name: &str) -> CompassResult<()> {
        if self
            .selection
            .as_ref()
            .is_some_and(|selection| !selection.tables.contains(name))
        {
            return Err(CompassError::AccessDenied(format!(
                "table '{name}' is outside this reader's selected tables"
            )));
        }
        Ok(())
    }

    pub(super) fn selected_tables(&self) -> Option<&BTreeSet<String>> {
        self.selection.as_ref().map(|s| &s.tables)
    }

    /// Coalesce concurrent cold requests from clones of this worker cache. Cancellation drops
    /// the lock so another caller can retry. Only verified bytes reach the published path.
    pub async fn ensure_shard<F, Fut>(&self, digest: &str, fetch: F) -> CompassResult<()>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = CompassResult<Vec<u8>>>,
    {
        // A warm entry never waits behind an unrelated cold download on the same stripe.
        if self.check_local_shard(digest).await? {
            return Ok(());
        }
        let stripe = usize::from_str_radix(&digest[..2], 16).expect("validated hex")
            % self.fetch_locks.len();
        let digest_lock = self.fetch_locks[stripe].clone().lock_owned().await;
        if self.check_local_shard(digest).await? {
            return Ok(());
        }
        let permit = self
            .cold_fills
            .clone()
            .acquire_owned()
            .await
            .expect("cache semaphore is never closed");
        let bytes = fetch().await?;
        let cache = self.clone();
        let expected = digest.to_owned();
        tokio::task::spawn_blocking(move || {
            // Keep both the capacity slot and this digest's lock through publication on cancellation.
            let _guards = (permit, digest_lock);
            cache.verify_and_publish(&expected, &bytes)
        })
        .await
        .map_err(|e| CompassError::Storage(format!("admit fetched shard: {e}")))??;
        Ok(())
    }

    async fn check_local_shard(&self, digest: &str) -> CompassResult<bool> {
        let cache = self.clone();
        let sha = digest.to_owned();
        match tokio::task::spawn_blocking(move || cache.has_verified(&sha))
            .await
            .map_err(|e| CompassError::Storage(format!("verify cached shard: {e}")))?
        {
            // A damaged entry may only be replaced by new bytes matching the original digest.
            Err(CompassError::IntegrityViolation(_)) => Ok(false),
            result => result,
        }
    }

    fn known(&self, digest: &str, stamp: FileStamp) -> bool {
        self.fingerprints
            .lock()
            .is_ok_and(|stamps| stamps.get(digest) == Some(&stamp))
    }

    fn remember(&self, digest: &str, stamp: Option<FileStamp>) {
        if let Some(stamp) = stamp
            && let Ok(mut stamps) = self.fingerprints.lock()
        {
            // Reaching the cap only causes a later rehash. It never admits unverified bytes.
            if stamps.len() >= 8192 && !stamps.contains_key(digest) {
                stamps.clear();
            }
            stamps.insert(digest.to_owned(), stamp);
        }
    }

    pub fn has_verified(&self, sha256: &str) -> CompassResult<bool> {
        let path = self.shard_path(sha256)?;
        let metadata = match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            result => {
                result.map_err(|e| CompassError::Storage(format!("stat cached shard: {e}")))?
            }
        };
        if !metadata.is_file() {
            return Err(CompassError::IntegrityViolation(format!(
                "cached shard {sha256} is not a regular file"
            )));
        }
        let stamp = FileStamp::new(&metadata);
        if stamp.is_some_and(|value| self.known(sha256, value)) {
            return Ok(true);
        }
        // Reopened or modified cache entries must prove their bytes before a reader uses them.
        let mut file = File::open(&path)
            .map_err(|e| CompassError::Storage(format!("open cached shard: {e}")))?;
        let mut hash = Sha256::new();
        std::io::copy(&mut file, &mut hash)
            .map_err(|e| CompassError::Storage(format!("hash cached shard: {e}")))?;
        let final_meta = file
            .metadata()
            .map_err(|e| CompassError::Storage(format!("stat verified shard: {e}")))?;
        if hex::encode(hash.finalize()) != sha256 || FileStamp::new(&final_meta) != stamp {
            return Err(CompassError::IntegrityViolation(format!(
                "corrupt or changed cached shard {sha256}"
            )));
        }
        self.remember(sha256, stamp);
        Ok(true)
    }

    /// Verify `bytes` against `expected_sha256` ONCE and write atomically into the cache.
    /// Subsequent lookups skip re-hashing (`B12`).
    pub fn ingest_bytes(&self, expected_sha256: &str, bytes: &[u8]) -> CompassResult<PathBuf> {
        let target = self.shard_path(expected_sha256)?;
        if self.has_verified(expected_sha256)? {
            return Ok(target);
        }
        self.verify_and_publish(expected_sha256, bytes)
    }

    fn verify_and_publish(&self, expected_sha256: &str, bytes: &[u8]) -> CompassResult<PathBuf> {
        let target = self.shard_path(expected_sha256)?;
        let actual = hex::encode(Sha256::digest(bytes));
        if actual != expected_sha256 {
            return Err(CompassError::IntegrityViolation(format!(
                "shard SHA-256 mismatch: expected {expected_sha256}, computed {actual}"
            )));
        }
        self.publish_verified(expected_sha256, bytes, target)
    }

    fn publish_verified(
        &self,
        expected_sha256: &str,
        bytes: &[u8],
        target: PathBuf,
    ) -> CompassResult<PathBuf> {
        let tmp = self.sha256_dir.join(format!(
            ".tmp-{}-{expected_sha256}.parquet",
            Uuid::new_v4().simple()
        ));
        std::fs::write(&tmp, bytes).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            CompassError::Storage(format!("write temp shard {}: {e}", tmp.display()))
        })?;
        std::fs::rename(&tmp, &target).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            CompassError::Storage(format!("commit cached shard {}: {e}", target.display()))
        })?;
        self.verified_ingests.fetch_add(1, Ordering::Relaxed);
        Ok(target)
    }

    /// Verify `src` against `expected_sha256` ONCE and store in the content-addressed cache.
    pub fn ingest_file(&self, expected_sha256: &str, src: &Path) -> CompassResult<PathBuf> {
        let target = self.shard_path(expected_sha256)?;
        if self.has_verified(expected_sha256)? {
            return Ok(target);
        }
        let bytes = std::fs::read(src).map_err(|e| {
            CompassError::IntegrityViolation(format!("read shard {}: {e}", src.display()))
        })?;
        self.ingest_bytes(expected_sha256, &bytes)
    }

    /// Compute SHA-256 of a newly written DuckLake file and archive it into `sha256/<digest>.parquet`.
    pub fn archive_new_file(&self, src: &Path) -> CompassResult<(String, u64)> {
        let bytes = std::fs::read(src).map_err(|e| {
            CompassError::Storage(format!("read DuckLake file {}: {e}", src.display()))
        })?;
        let digest = hex::encode(Sha256::digest(&bytes));
        let size = bytes.len() as u64;
        if !self.has_verified(&digest)? {
            self.publish_verified(&digest, &bytes, self.shard_path(&digest)?)?;
        }
        Ok((digest, size))
    }
}
