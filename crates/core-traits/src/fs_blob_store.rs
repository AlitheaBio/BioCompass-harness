//! POSIX filesystem implementation of `BlobStore` and local OCI reference verifier.

use crate::traits::{BlobStore, OciRegistryClient};
use crate::types::{CompassError, CompassResult, OciImageRef};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

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
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| CompassError::Storage(format!("create blob parent: {e}")))?;
        }
        let digest = hex::encode(Sha256::digest(bytes));
        tokio::fs::write(&target, bytes)
            .await
            .map_err(|e| CompassError::Storage(format!("write blob '{key}': {e}")))?;
        Ok(digest)
    }

    async fn get_bytes(&self, key: &str) -> CompassResult<Vec<u8>> {
        let target = self.resolve_safe_key(key)?;
        if !target.exists() {
            return Err(CompassError::NotFound(format!("blob '{key}'")));
        }
        tokio::fs::read(&target)
            .await
            .map_err(|e| CompassError::Storage(format!("read blob '{key}': {e}")))
    }

    async fn exists(&self, key: &str) -> CompassResult<bool> {
        let target = self.resolve_safe_key(key)?;
        Ok(target.is_file())
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
