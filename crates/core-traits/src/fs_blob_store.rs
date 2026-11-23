//! POSIX filesystem implementation of `BlobStore` and local OCI reference verifier.

use crate::traits::{BlobStore, OciRegistryClient};
use crate::types::{CompassError, CompassResult, OciImageRef};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use tokio::io::AsyncWriteExt;

const STAGING_DIR: &str = ".biocompass-blob-staging";

/// Local POSIX filesystem blob store (`~/.biocompass/store`) strictly rejecting path traversal.
#[derive(Debug, Clone)]
pub struct LocalFsBlobStore {
    root: PathBuf,
}

impl LocalFsBlobStore {
    /// Initialize a POSIX blob store rooted at `root`.
    pub fn new(root: impl AsRef<Path>) -> CompassResult<Self> {
        let root_path = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root_path)
            .map_err(|e| CompassError::Storage(format!("create blob root {}: {e}", root_path.display())))?;
        Ok(Self { root: root_path })
    }

    // ENOENT at a leaf can also be caused by a dangling symlink above it. A
    // missing receipt may release a reservation, so validate each managed directory.
    async fn parents_exist(&self, target: &Path) -> CompassResult<bool> {
        let root_meta = tokio::fs::metadata(&self.root).await
            .map_err(|e| CompassError::Storage(format!("inspect blob store root: {e}")))?;
        if !root_meta.is_dir() {
            return Err(CompassError::IntegrityViolation("blob store root is not a directory".into()));
        }
        let relative = target.strip_prefix(&self.root)
            .map_err(|_| CompassError::Validation("blob path outside store root".into()))?;
        let mut path = self.root.clone();
        if let Some(parent) = relative.parent() {
            for part in parent.components() {
                path.push(part);
                match tokio::fs::symlink_metadata(&path).await {
                    Ok(meta) if meta.is_dir() => {}
                    Ok(_) => return Err(CompassError::IntegrityViolation(format!(
                        "non-directory or symlink in managed blob path: {}", path.display()
                    ))),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                    Err(e) => return Err(CompassError::Storage(format!("inspect managed blob directory: {e}"))),
                }
            }
        }
        Ok(true)
    }

    fn resolve_safe_key(&self, key: &str) -> CompassResult<PathBuf> {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return Err(CompassError::Validation("blob key cannot be empty".into()));
        }
        if trimmed.starts_with('/') || trimmed.contains('\\') || trimmed.contains('\0') {
            return Err(CompassError::Validation(format!(
                "blob key '{key}' must be a relative POSIX key without leading '/', '\\', or NUL bytes"
            )));
        }
        let rel = Path::new(trimmed);
        if rel.components().next() == Some(Component::Normal(STAGING_DIR.as_ref())) {
            return Err(CompassError::Validation("internal blob staging path".into()));
        }
        let mut target = self.root.clone();
        for comp in rel.components() {
            match comp {
                Component::Normal(part) => target.push(part),
                _ => {
                    return Err(CompassError::Validation(format!(
                        "unsafe path component in blob key '{key}'"
                    )));
                }
            }
        }
        Ok(target)
    }
}

#[async_trait]
impl BlobStore for LocalFsBlobStore {
    async fn put_bytes(&self, key: &str, bytes: &[u8]) -> CompassResult<String> {
        let target = self.resolve_safe_key(key)?;
        if !self.parents_exist(&target).await? && let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| CompassError::Storage(format!("create blob parent: {e}")))?;
        }
        // Keep interrupted writes outside run artifact directories. A killed process can
        // leave a unique temporary file, but readers see only complete, renamed objects.
        let staging = self.root.join(STAGING_DIR);
        tokio::fs::create_dir_all(&staging).await
            .map_err(|e| CompassError::Storage(format!("create blob staging directory: {e}")))?;
        let temp = tempfile::NamedTempFile::new_in(&staging)
            .map_err(|e| CompassError::Storage(format!("create staging file for '{key}': {e}")))?;
        let handle = temp.reopen()
            .map_err(|e| CompassError::Storage(format!("open staging file for '{key}': {e}")))?;
        let mut file = tokio::fs::File::from_std(handle);
        file.write_all(bytes).await
            .map_err(|e| CompassError::Storage(format!("write blob '{key}': {e}")))?;
        file.flush().await
            .map_err(|e| CompassError::Storage(format!("flush blob '{key}': {e}")))?;
        drop(file);
        temp.persist(&target)
            .map_err(|e| CompassError::Storage(format!("publish blob '{key}': {e}")))?;
        let digest = hex::encode(Sha256::digest(bytes));
        Ok(digest)
    }

    async fn get_bytes(&self, key: &str) -> CompassResult<Vec<u8>> {
        let target = self.resolve_safe_key(key)?;
        if !self.parents_exist(&target).await? {
            return Err(CompassError::NotFound(format!("blob '{key}'")));
        }
        match tokio::fs::symlink_metadata(&target).await {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Err(CompassError::IntegrityViolation(format!("blob '{key}' is not a regular file"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(CompassError::NotFound(format!("blob '{key}'")));
            }
            Err(e) => return Err(CompassError::Storage(format!("stat blob '{key}': {e}"))),
        }
        tokio::fs::read(&target).await.map_err(|e| CompassError::Storage(format!("read blob '{key}': {e}")))
    }

    async fn exists(&self, key: &str) -> CompassResult<bool> {
        let target = self.resolve_safe_key(key)?;
        Ok(target.is_file())
    }

    async fn list_keys_under(&self, directory: &str) -> CompassResult<Vec<String>> {
        let root = self.resolve_safe_key(directory)?;
        if !self.parents_exist(&root).await? {
            return Ok(Vec::new());
        }
        match tokio::fs::symlink_metadata(&root).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(CompassError::Storage(format!("stat blob directory '{directory}': {e}"))),
            Ok(meta) if !meta.is_dir() => return Err(CompassError::IntegrityViolation(format!(
                "blob directory '{directory}' is not a regular directory"
            ))),
            Ok(_) => {}
        }
        let mut pending = vec![(directory.to_owned(), root)];
        let mut keys = Vec::new();
        while let Some((prefix, dir)) = pending.pop() {
            let mut entries = tokio::fs::read_dir(dir).await
                .map_err(|e| CompassError::Storage(format!("list blobs under '{prefix}': {e}")))?;
            while let Some(entry) = entries.next_entry().await
                .map_err(|e| CompassError::Storage(format!("read blob entry under '{prefix}': {e}")))? {
                let name = entry.file_name().into_string().map_err(|_| {
                    CompassError::IntegrityViolation(format!("non-UTF-8 blob key under '{prefix}'"))
                })?;
                let key = format!("{prefix}/{name}");
                let kind = entry.file_type().await
                    .map_err(|e| CompassError::Storage(format!("stat blob '{key}': {e}")))?;
                if kind.is_dir() {
                    pending.push((key, entry.path()));
                } else if kind.is_file() {
                    keys.push(key);
                } else {
                    return Err(CompassError::IntegrityViolation(format!("non-regular blob '{key}'")));
                }
            }
        }
        keys.sort();
        Ok(keys)
    }

    async fn materialize_to_path(&self, key: &str, dest: &Path) -> CompassResult<u64> {
        let src = self.resolve_safe_key(key)?;
        if !src.is_file() {
            return Err(CompassError::NotFound(format!("blob '{key}'")));
        }
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| CompassError::Storage(format!("create stage dest dir: {e}")))?;
        }
        tokio::fs::copy(&src, dest)
            .await
            .map_err(|e| CompassError::Storage(format!("copy blob '{key}' -> {}: {e}", dest.display())))
    }
}

/// Standard OCI reference policy verifier supporting immutable `@sha256` references and optional `local/*` dev images.
#[derive(Debug, Clone)]
pub struct StandardOciPolicyVerifier {
    pub allow_local_dev_tags: bool,
}

impl StandardOciPolicyVerifier {
    /// Strict production/cloud mode requiring `@sha256:<64-hex>` digests on every plugin image.
    pub const fn strict_pinned() -> Self {
        Self {
            allow_local_dev_tags: false,
        }
    }

    /// Local workstation mode permitting `local/<plugin>:<tag>` during iterative development alongside `@sha256` images.
    pub const fn workstation_dev() -> Self {
        Self {
            allow_local_dev_tags: true,
        }
    }
}

#[async_trait]
impl OciRegistryClient for StandardOciPolicyVerifier {
    async fn verify_image(&self, image_raw: &str) -> CompassResult<OciImageRef> {
        OciImageRef::parse(image_raw, self.allow_local_dev_tags)
    }
}

#[cfg(test)]
mod recovery_blob_tests {
    use super::*;
    use tempfile::tempdir;

    #[cfg(unix)]
    #[tokio::test]
    async fn dangling_parent_is_not_a_missing_receipt() -> CompassResult<()> {
        let temp = tempdir().unwrap();
        let blobs = LocalFsBlobStore::new(temp.path())?;
        let chain = "runs/pending/froglet_chain.json";
        assert!(matches!(blobs.get_bytes(chain).await, Err(CompassError::NotFound(_))));
        std::fs::create_dir(temp.path().join("runs")).unwrap();
        std::os::unix::fs::symlink("lost-run", temp.path().join("runs/pending")).unwrap();
        assert!(matches!(blobs.get_bytes(chain).await, Err(CompassError::IntegrityViolation(_))));
        Ok(())
    }

    #[tokio::test]
    async fn readers_observe_only_one_whole_published_version() -> CompassResult<()> {
        let temp = tempdir().unwrap();
        let blobs = LocalFsBlobStore::new(temp.path())?;
        let key = "runs/atomic/artifacts/large.dat";
        let old = vec![0x33; 1 << 18];
        let new = vec![0xcc; old.len()];
        blobs.put_bytes(key, &old).await?;
        let writer = async {
            for body in [&new, &old, &new, &old, &new] {
                blobs.put_bytes(key, body).await?;
            }
            Ok::<(), CompassError>(())
        };
        let reader = async {
            for _ in 0..40 {
                let bytes = blobs.get_bytes(key).await?;
                assert!(bytes == old || bytes == new);
            }
            Ok::<(), CompassError>(())
        };
        tokio::try_join!(writer, reader)?;
        assert_eq!(blobs.get_bytes(key).await?, new);
        std::fs::write(temp.path().join(STAGING_DIR).join("left-by-terminated-process"), b"truncated").unwrap();
        assert_eq!(blobs.list_keys_under("runs/atomic/artifacts").await?, vec![key]);
        Ok(())
    }
}
