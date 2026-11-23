//! Native authorization and durable audit storage for reviewed live reads.
use super::{App, Error, User, query_access, runs, store::Release};
use async_trait::async_trait;
use biocompass_core_traits::{CompassError, CompassResult, DatasetGrant, RunRecord};
use biocompass_data_engine::DuckLakeCatalogEngine;
use biocompass_node_runtime::{LiveQueryAuthority, LiveQueryBinding, LiveQuerySession};
use std::{fs, io::Write, path::{Path, PathBuf}, sync::Arc};

struct Authority {
    app: App,
    user: User,
    dataset: String,
    version: u64,
    grant: DatasetGrant,
    audit_path: PathBuf,
}

pub fn session(
    app: &App, user: &User, dataset: &str, release: &Release,
    catalog: DuckLakeCatalogEngine, run: &RunRecord, declared: Vec<String>,
) -> Result<LiveQuerySession, Error> {
    let snapshot = release.manifest["snapshot_id"].as_u64()
        .ok_or_else(|| Error("storage: missing release snapshot".into()))?;
    let digest = release.manifest["dataset_digest"].as_str()
        .ok_or_else(|| Error("storage: missing release digest".into()))?.to_owned();
    let max_input = run.quote.froglet_quote.pointer("/payload/execution_limits/max_input_bytes")
        .and_then(serde_json::Value::as_u64).and_then(|size| usize::try_from(size).ok())
        .ok_or_else(|| Error("storage: missing signed query input limit".into()))?;
    let param_size = serde_json::to_vec(&run.params)
        .map_err(|_| Error("storage: invalid run parameters".into()))?.len();
    let audit_path = app.store.dataset_directory(dataset)?
        .join("run-query-audit").join(format!("{}.json", run.run_id));
    let grant = run.grant.clone().ok_or_else(|| Error("storage: missing run grant".into()))?;
    LiveQuerySession::new(
        run,
        LiveQueryBinding { catalog, version: release.version, snapshot, digest },
        declared,
        max_input.saturating_sub(param_size),
        Arc::new(Authority {
            app: app.clone(), user: user.clone(), dataset: dataset.into(),
            version: release.version, grant, audit_path,
        }),
    ).map_err(runs::run_error)
}

#[async_trait]
impl LiveQueryAuthority for Authority {
    async fn ensure_allowed(&self) -> CompassResult<()> {
        let allowed = runs::access(&self.app, &self.user, &self.dataset, self.version)
            .ok().is_some_and(|(_, current, _)| query_access::supports(&current, &self.grant));
        if allowed {
            Ok(())
        } else {
            Err(CompassError::AccessDenied(
                "DATASET_ACCESS_REVOKED: live dataset read stopped".into(),
            ))
        }
    }

    async fn save_audit(&self, document: &[u8]) -> CompassResult<()> {
        let path = self.audit_path.clone();
        let document = document.to_vec();
        tokio::task::spawn_blocking(move || persist(&path, &document)).await
            .map_err(|_| CompassError::Storage("live query audit writer failed".into()))?
    }
}

fn persist(path: &Path, document: &[u8]) -> CompassResult<()> {
    let storage = |error: std::io::Error| CompassError::Storage(format!("write live query audit: {error}"));
    let dir = path.parent().expect("audit path has parent");
    fs::create_dir_all(dir).map_err(storage)?;
    let mut draft = tempfile::NamedTempFile::new_in(dir).map_err(storage)?;
    draft.write_all(document).map_err(storage)?;
    draft.as_file().sync_all().map_err(storage)?;
    draft.persist(path).map_err(|error| storage(error.error))?;
    fs::File::open(dir).and_then(|file| file.sync_all()).map_err(storage)
}
