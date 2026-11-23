//! Byte admission for a single worker's explicitly labelled, dedicated Docker daemon.
//! Accounts Docker's logical image sizes once per config ID. Shared layers are counted
//! conservatively across distinct IDs. Docker may stage one cold pull before its size is known.
use super::{CompassError, CompassResult, EngineKind, OciContainerRunner, OciImageRef};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::{process::Command, sync::Mutex};

type Lease = Arc<()>;
const OWNER: &str = "io.biocompass.worker-cache=";

// Store just image identities, sizes and access order. Removed images keep a
// bounded, platform-specific size hint so an impossible retry avoids a pull.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct SeenImage {
    id: String,
    bytes: u64,
    serial: u64,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct Usage {
    scope: String,
    serial: u64,
    touches: BTreeMap<String, u64>,
    known: BTreeMap<String, SeenImage>,
    #[serde(skip)]
    leases: BTreeMap<String, Weak<()>>,
}

impl Usage {
    async fn load(path: &Path, scope: String) -> CompassResult<Self> {
        match tokio::fs::read(path).await {
            Ok(raw) => {
                let existing: Self = serde_json::from_slice(&raw)
                    .map_err(|e| error(format!("invalid worker image history: {e}")))?;
                if existing.scope == scope {
                    return Ok(existing);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(error(e.to_string())),
        }
        Ok(Self {
            scope,
            ..Self::default()
        })
    }

    async fn save(&self, path: &Path) -> CompassResult<()> {
        let temporary = path.with_extension("tmp");
        let bytes = serde_json::to_vec(&self).map_err(|e| error(e.to_string()))?;
        // Each worker's state claim has one writer. A restart can overwrite an
        // incomplete temporary file; the previous complete generation stays readable.
        let save = async {
            let mut file = tokio::fs::File::create(&temporary).await?;
            file.write_all(&bytes).await?;
            file.sync_all().await?;
            tokio::fs::rename(&temporary, path).await?;
            if let Some(parent) = path.parent() {
                tokio::fs::File::open(parent).await?.sync_all().await?;
            }
            Ok::<_, std::io::Error>(())
        };
        save.await
            .map_err(|e| error(format!("save worker image history: {e}")))
    }

    fn remember(&mut self, digest: &str, image: &Image) {
        self.serial = self.serial.saturating_add(1);
        self.known.insert(
            digest.to_owned(),
            SeenImage {
                id: image.id.clone(),
                bytes: image.size,
                serial: self.serial,
            },
        );
        if self.known.len() > 128 {
            let oldest = self
                .known
                .iter()
                .min_by_key(|(_, entry)| entry.serial)
                .map(|(digest, _)| digest.to_owned())
                .expect("nonempty history");
            self.known.remove(&oldest);
        }
    }
}

#[derive(Debug)]
pub(super) struct WorkerCache {
    label: String,
    limit: u64,
    history_path: PathBuf,
    protected: BTreeSet<String>,
    usage: Mutex<Usage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Image {
    id: String,
    size: u64,
    created: String,
    repo_digests: Option<Vec<String>>,
    repo_tags: Option<Vec<String>>,
}

fn error(message: impl Into<String>) -> CompassError {
    CompassError::ContainerEngine(message.into())
}

async fn cli(runner: &OciContainerRunner, args: &[&str]) -> CompassResult<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        Command::new(&runner.engine.binary_path)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| error("worker cache Docker inspection timed out"))?
    .map_err(|e| error(e.to_string()))?;
    if !output.status.success() {
        return Err(error(format!(
            "worker cache Docker {} failed: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    String::from_utf8(output.stdout).map_err(|e| error(e.to_string()))
}

async fn inspect(runner: &OciContainerRunner) -> CompassResult<Vec<Image>> {
    let ids = cli(runner, &["image", "ls", "-aq", "--no-trunc"]).await?;
    let unique: BTreeSet<_> = ids.split_whitespace().collect();
    let mut all = Vec::new();
    let ids: Vec<_> = unique.into_iter().collect();
    for chunk in ids.chunks(128) {
        let mut args = vec!["image", "inspect"];
        args.extend_from_slice(chunk);
        let data = cli(runner, &args).await?;
        all.extend(serde_json::from_str::<Vec<Image>>(&data).map_err(|e| error(e.to_string()))?);
    }
    Ok(all)
}

async fn container_images(runner: &OciContainerRunner) -> CompassResult<BTreeSet<String>> {
    let list = cli(runner, &["ps", "-aq", "--no-trunc"]).await?;
    let mut ids = BTreeSet::new();
    for container in list.split_whitespace() {
        // If a container disappears between list and inspect, retry admission on the
        // next call. Never guess whether an image may be evicted.
        ids.insert(
            cli(
                runner,
                &["container", "inspect", "--format", "{{.Image}}", container],
            )
            .await?
            .trim()
            .to_owned(),
        );
    }
    Ok(ids)
}

impl WorkerCache {
    async fn check_owner(&self, runner: &OciContainerRunner) -> CompassResult<()> {
        let raw = cli(runner, &["info", "--format", "{{json .Labels}}"]).await?;
        let labels: Option<Vec<String>> =
            serde_json::from_str(raw.trim()).map_err(|e| error(e.to_string()))?;
        if !labels.unwrap_or_default().contains(&self.label) {
            return Err(error(
                "image cache refused: Docker daemon lacks this worker's ownership label",
            ));
        }
        Ok(())
    }

    async fn capacity(
        &self,
        runner: &OciContainerRunner,
        usage: &mut Usage,
        all: &[Image],
        requested: Option<&str>,
        // For a previously measured cold image, check feasibility before going
        // to the registry. This check never evicts on behalf of a failed pull.
        pending_size: Option<u64>,
    ) -> CompassResult<()> {
        let ids: BTreeSet<_> = all.iter().map(|i| i.id.as_str()).collect();
        usage.touches.retain(|id, _| ids.contains(id.as_str()));
        let mut bytes = all.iter().map(|i| i.size).sum::<u64>() + pending_size.unwrap_or(0);
        if bytes <= self.limit {
            return Ok(());
        }
        let occupied = container_images(runner).await?;
        let mut eligible: Vec<_> = all
            .iter()
            .filter(|i| {
                let refs = i.repo_digests.as_deref().unwrap_or_default();
                !refs.is_empty()
                    && i.repo_tags.as_ref().is_none_or(Vec::is_empty)
                    && Some(i.id.as_str()) != requested
                    && !occupied.contains(&i.id)
                    && usage
                        .leases
                        .get(&i.id)
                        .is_none_or(|lease| lease.strong_count() == 0)
                    && !refs
                        .iter()
                        .filter_map(|r| r.split_once('@'))
                        .any(|(_, d)| self.protected.contains(d))
            })
            .collect();
        let removable: u64 = eligible.iter().map(|i| i.size).sum();
        if bytes - removable > self.limit {
            return Err(error(format!(
                "worker image cache full: {} bytes are required or protected, limit {} bytes",
                bytes - removable,
                self.limit
            )));
        }
        if pending_size.is_some() {
            return Ok(());
        }
        eligible.sort_by_key(|i| {
            (
                usage.touches.get(&i.id).copied().unwrap_or(0),
                &i.created,
                &i.id,
            )
        });
        for image in eligible {
            if bytes <= self.limit {
                break;
            }
            for reference in image.repo_digests.as_ref().expect("eligible digest") {
                cli(runner, &["image", "rm", "--no-prune", reference]).await?;
            }
            bytes -= image.size;
            usage.touches.remove(&image.id);
            tracing::info!(image_id=%image.id, bytes=image.size, "evicted worker image");
        }
        if inspect(runner).await?.iter().map(|i| i.size).sum::<u64>() > self.limit {
            return Err(error(
                "Docker retained more logical image bytes than the worker cache limit",
            ));
        }
        Ok(())
    }

    async fn admit(&self, runner: &OciContainerRunner, reference: &str) -> CompassResult<Lease> {
        OciImageRef::parse(reference, false)?;
        let mut usage = self.usage.lock().await;
        self.check_owner(runner).await?;
        let digest = reference.split_once('@').expect("validated digest").1;
        let present = runner.has_verified_local_digest(reference).await;
        // A failed native-platform probe can also mean an existing image is for a
        // different architecture. Never remove a pre-existing reference on rollback.
        let already_stored = if present {
            true
        } else {
            let inventory = inspect(runner).await?;
            let stored = inventory.iter().any(|image| {
                image
                    .repo_digests
                    .iter()
                    .flatten()
                    .any(|r| r.split_once('@').is_some_and(|(_, d)| d == digest))
            });
            if let Some(seen) = usage.known.get(digest).cloned() {
                let additional = if inventory.iter().any(|i| i.id == seen.id) {
                    0
                } else {
                    seen.bytes
                };
                self.capacity(
                    runner,
                    &mut usage,
                    &inventory,
                    Some(&seen.id),
                    Some(additional),
                )
                .await?;
            }
            runner.pull_uncached(reference).await?;
            stored
        };
        let result = async {
            if !present && !runner.has_verified_local_digest(reference).await {
                return Err(error(
                    "Docker did not retain the requested digest for its native platform",
                ));
            }
            let inventory = inspect(runner).await?;
            let image = inventory
                .iter()
                .find(|i| {
                    i.repo_digests
                        .iter()
                        .flatten()
                        .any(|r| r.split_once('@').is_some_and(|(_, d)| d == digest))
                })
                .ok_or_else(|| {
                    error("requested immutable image is absent from Docker's inventory")
                })?;
            let id = image.id.clone();
            usage.remember(digest, image);
            self.capacity(runner, &mut usage, &inventory, Some(&id), None)
                .await?;
            let serial = usage.serial;
            usage.touches.insert(id.clone(), serial);
            let lease = usage
                .leases
                .get(&id)
                .and_then(Weak::upgrade)
                .unwrap_or_default();
            usage.leases.retain(|_, lease| lease.strong_count() > 0);
            usage.leases.insert(id, Arc::downgrade(&lease));
            Ok(lease)
        }
        .await;
        if result.is_err() && !already_stored {
            // Only roll back the newly fetched digest. Never force-delete an active image.
            cli(runner, &["image", "rm", "--no-prune", reference]).await?;
        }
        usage.save(&self.history_path).await?;
        result
    }
}

impl OciContainerRunner {
    /// Bind cache custody to the explicitly named nested engine. This is opt-in;
    /// generic local, Podman and operator-provided socket runners never invoke it.
    pub async fn with_dedicated_worker_cache(
        mut self,
        owner: &str,
        bytes: u64,
        protected: &[String],
        state_dir: &Path,
    ) -> CompassResult<Self> {
        if self.engine.kind != EngineKind::Docker || bytes == 0 || owner.is_empty() {
            return Err(error(
                "a dedicated worker image cache requires Docker, an owner label and a positive byte limit",
            ));
        }
        let mut digests = BTreeSet::new();
        for reference in protected {
            OciImageRef::parse(reference, false)?;
            let (_, digest) = reference
                .split_once('@')
                .ok_or_else(|| error("cache requires a digest-pinned image"))?;
            digests.insert(digest.to_owned());
        }
        let daemon = cli(
            &self,
            &["info", "--format", "{{.ID}}/{{.OSType}}/{{.Architecture}}"],
        )
        .await?;
        let history_path = state_dir.join("image-cache-history.json");
        let history = Usage::load(&history_path, format!("{owner}:{}", daemon.trim())).await?;
        let cache = Arc::new(WorkerCache {
            label: format!("{OWNER}{owner}"),
            limit: bytes,
            history_path,
            protected: digests,
            usage: Mutex::new(history),
        });
        cache.check_owner(&self).await?;
        {
            let mut usage = cache.usage.lock().await;
            let inventory = inspect(&self).await?;
            cache
                .capacity(&self, &mut usage, &inventory, None, None)
                .await?;
            usage.save(&cache.history_path).await?;
        }
        self.worker_cache = Some(cache);
        for reference in protected {
            self.prepare_for_execution(reference).await?;
        }
        Ok(self)
    }

    pub(super) async fn prepare_for_execution(
        &self,
        reference: &str,
    ) -> CompassResult<Option<Lease>> {
        if let Some(cache) = &self.worker_cache {
            // If a run is cancelled during Docker pull, finish testing its size and
            // rolling back any over-budget image. A detached task never executes a module.
            let cache = Arc::clone(cache);
            let runner = self.clone();
            let reference = reference.to_owned();
            return tokio::spawn(async move { cache.admit(&runner, &reference).await })
                .await
                .map_err(|e| error(e.to_string()))?
                .map(Some);
        }
        if !self.has_verified_local_digest(reference).await {
            self.pull_uncached(reference).await?;
            if !self.has_verified_local_digest(reference).await {
                return Err(error(format!(
                    "engine did not retain the requested image digest for its native platform: {reference}"
                )));
            }
        }
        Ok(None)
    }
}
