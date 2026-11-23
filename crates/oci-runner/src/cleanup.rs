//! Bounded Docker/Podman cleanup. Native workspaces identify their own jobs on restart.
use super::*;
use sha2::{Digest, Sha256};
use std::io;

const CLEANUP_DEADLINE: Duration = Duration::from_secs(5);
const WORKSPACE_LABEL: &str = "bio.alithea.biocompass.workspace";

/// A concrete engine object. Removal uses its immutable ID so a reused name is safe.
pub struct AbandonedContainer {
    run_id: RunId,
    engine_id: String,
    scope: String,
}

impl AbandonedContainer {
    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }
    pub fn engine_id(&self) -> &str {
        &self.engine_id
    }
}

impl OciContainerRunner {
    pub fn for_native_workspace(&self, stage_root: &Path, provider: &str) -> CompassResult<Self> {
        let root = stage_root
            .canonicalize()
            .map_err(|e| CompassError::Storage(e.to_string()))?;
        let mut hash = Sha256::new();
        hash.update(provider.as_bytes());
        hash.update([0]);
        hash.update(root.as_os_str().as_encoded_bytes());
        let mut runner = self.clone();
        runner.workspace_scope = Some(hex::encode(hash.finalize()));
        Ok(runner)
    }

    pub(super) fn container_name(&self, run: &RunId) -> String {
        match &self.workspace_scope {
            Some(scope) => format!("biocompass-job-{scope}-{}", hex::encode(run.as_str())),
            None => format!("biocompass-job-{}", Uuid::new_v4().simple()),
        }
    }

    pub(super) fn label_native_job(&self, args: &mut Vec<String>) {
        if let Some(scope) = &self.workspace_scope {
            args.splice(
                1..1,
                ["--label".into(), format!("{WORKSPACE_LABEL}={scope}")],
            );
        }
    }

    /// Discover owned jobs only. The caller must also find the persisted run and hold
    /// its confirmation lease before removing any returned object.
    pub async fn native_job_containers(&self) -> CompassResult<Vec<AbandonedContainer>> {
        let scope = self.workspace_scope.as_ref().ok_or_else(|| {
            CompassError::Validation("native container discovery requires a workspace".into())
        })?;
        let filter = format!("label={WORKSPACE_LABEL}={scope}");
        let stdout = self
            .cleanup_command(&[
                "ps",
                "-a",
                "--no-trunc",
                "--filter",
                &filter,
                "--format",
                "{{.ID}} {{.Names}}",
            ])
            .await?;
        let listing = std::str::from_utf8(&stdout).map_err(|e| {
            CompassError::ContainerEngine(format!("invalid container inventory: {e}"))
        })?;
        let prefix = format!("biocompass-job-{scope}-");
        let mut containers = Vec::new();
        for line in listing.lines() {
            let Some((id, name)) = line.split_once(' ') else {
                continue;
            };
            if !is_engine_id(id) {
                continue;
            }
            let Some(encoded) = name.strip_prefix(&prefix) else {
                continue;
            };
            let Some(run_id) = hex::decode(encoded)
                .ok()
                .and_then(|v| String::from_utf8(v).ok())
                .and_then(|v| RunId::new(v).ok())
            else {
                continue;
            };
            if self.container_name(&run_id) == name {
                containers.push(AbandonedContainer {
                    run_id,
                    engine_id: id.to_owned(),
                    scope: scope.clone(),
                });
            }
        }
        Ok(containers)
    }

    pub async fn remove_abandoned_container(
        &self,
        container: &AbandonedContainer,
    ) -> CompassResult<()> {
        if self.workspace_scope.as_ref() != Some(&container.scope) {
            return Err(CompassError::Validation(
                "cannot remove a container discovered by another workspace".into(),
            ));
        }
        self.force_remove_container(&container.engine_id).await
    }

    pub(super) async fn force_remove_container(&self, name: &str) -> CompassResult<()> {
        self.cleanup_command(&["rm", "-f", name])
            .await
            .map(|_| ())
            .map_err(|e| {
                CompassError::ContainerEngine(format!(
                    "OCI_CLEANUP_UNCONFIRMED: removal of {name} was not confirmed: {e}"
                ))
            })
    }

    async fn cleanup_command(&self, args: &[&str]) -> CompassResult<Vec<u8>> {
        let mut child = Command::new(&self.engine.binary_path)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                CompassError::ContainerEngine(format!(
                    "spawn {} {}: {e}",
                    self.engine.kind.as_str(),
                    args[0]
                ))
            })?;
        let stdout = child.stdout.take().expect("stdout configured as a pipe");
        let read = async {
            // Inventory is scoped to one workspace. Abort on oversized CLI output.
            const LIMIT: u64 = 8 * 1024 * 1024;
            let mut bytes = Vec::new();
            stdout.take(LIMIT + 1).read_to_end(&mut bytes).await?;
            if bytes.len() as u64 > LIMIT {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "container inventory exceeds 8 MiB",
                ));
            }
            Ok(bytes)
        };
        match tokio::time::timeout(CLEANUP_DEADLINE, async {
            tokio::try_join!(child.wait(), read)
        })
        .await
        {
            Ok(Ok((status, data))) if status.success() => Ok(data),
            Ok(Ok((status, _))) => Err(CompassError::ContainerEngine(format!(
                "{} {} exited with {status}",
                self.engine.kind.as_str(),
                args[0]
            ))),
            outcome => {
                let reason = match outcome {
                    Err(_) => format!(
                        "{} {} exceeded {}s",
                        self.engine.kind.as_str(),
                        args[0],
                        CLEANUP_DEADLINE.as_secs()
                    ),
                    Ok(Err(e)) => e.to_string(),
                    _ => unreachable!(),
                };
                // The client and its daemon-owned container have independent lifetimes.
                stop_client(&mut child).await.map_err(|e| {
                    CompassError::ContainerEngine(format!("{reason}; reap engine client: {e}"))
                })?;
                Err(CompassError::ContainerEngine(reason))
            }
        }
    }

    pub(super) async fn abort_execution(
        &self,
        child: &mut tokio::process::Child,
        container_name: &str,
        reason: CompassError,
    ) -> CompassError {
        // The attached run client and the daemon-owned container have independent lifetimes.
        // Even if killing the client fails, attempt to remove the actual container.
        let client = stop_client(child).await;
        let removal = self.force_remove_container(container_name).await;
        if let Err(error) = removal {
            return CompassError::ContainerEngine(format!("{reason}; {error}"));
        }
        if let Err(error) = client {
            return CompassError::ContainerEngine(format!(
                "{reason}; reap attached client: {error}"
            ));
        }
        reason
    }
}

async fn stop_client(child: &mut tokio::process::Child) -> io::Result<()> {
    child.start_kill()?;
    // If the OS itself delays exit, drop to Tokio's child reaper and report the
    // uncertainty rather than keeping a failed run's reservation indefinitely.
    tokio::time::timeout(Duration::from_secs(1), child.wait())
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "OS has not confirmed engine client exit",
            )
        })??;
    Ok(())
}

fn is_engine_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
