//! Authorize private releases, check their input capabilities and dispatch a native signed run.
use super::{App, Error, User, blocking, query_access, run_queries, store::Release};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Response},
};
use biocompass::state::{AppState, DeploymentMode};
use biocompass_core_traits::{
    CatalogId, CompassError, DatasetGrant, OrgId, PluginId, PrepareRunRequest, RunId, RunQuote,
    RunRecord, RunStatus, ScopedQueryDefinition,
};
use biocompass_data_engine::{DuckLakeCatalogEngine, scoped_join::ScopeRelation};
use biocompass_node_runtime::validate_reviewed_hla_capabilities;
use biocompass_plugin_spec::{LoadedPlugin, reviewed_hla_input_contract};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path as FsPath, PathBuf},
    sync::Arc,
};

const TARGET_VALIDATION: &str = "alithea/bc-target-validation";

#[derive(Clone)]
pub struct PrivateRuns {
    native: Arc<AppState>,
    versions: HashMap<PluginId, String>,
    runtime_queries: HashMap<(PluginId, String), Vec<String>>,
    runtime_joins: HashMap<(PluginId, String), BTreeMap<String, ScopedQueryDefinition>>,
}

impl PrivateRuns {
    pub async fn load<const N: usize>(
        root: &FsPath,
        modules: [(&str, Option<PathBuf>); N],
        scoped_query_modules: Vec<PathBuf>,
    ) -> Result<Self, String> {
        let mut reviewed = Vec::new();
        for (expected_id, module) in modules {
            let Some(module) = module else { continue };
            let plugin = LoadedPlugin::load_from_dir(&module, false).map_err(|e| e.to_string())?;
            let expected_contract = reviewed_hla_input_contract(&plugin.manifest.id);
            if plugin.manifest.id.as_str() != expected_id
                || expected_contract.is_none()
                || plugin.manifest.dataset_input_contract.as_deref() != expected_contract
                || !plugin.manifest.network.is_airgapped()
                || !plugin.manifest.runtime_queries.iter().all(|query| {
                    biocompass_node_runtime::is_runtime_read(query)
                })
            {
                return Err(format!("private module {expected_id} requires its reviewed input contract and network isolation"));
            }
            let image = plugin.manifest.validate(false).map_err(|e| e.to_string())?;
            reviewed.push((plugin, image));
        }
        for directory in scoped_query_modules {
            let plugin = LoadedPlugin::load_from_dir(&directory, false).map_err(|error| error.to_string())?;
            if plugin.manifest.runtime_join_queries.is_empty() {
                return Err(format!("scoped query module '{}' has no declared joins", plugin.manifest.id));
            }
            let image = plugin.manifest.validate(false).map_err(|error| error.to_string())?;
            reviewed.push((plugin, image));
        }
        let native = AppState::bootstrap(root, DeploymentMode::Local, false)
            .await
            .map_err(|e| e.to_string())?;
        let runtime = native
            .local_runtime
            .as_ref()
            .expect("local bootstrap creates runtime");
        let mut versions = HashMap::new();
        let mut runtime_queries = HashMap::new();
        let mut runtime_joins = HashMap::new();
        for (plugin, image) in reviewed {
            let id = plugin.manifest.id.clone();
            let version = plugin.manifest.version.clone();
            if versions.contains_key(&id) {
                return Err(format!("duplicate reviewed module ID '{id}'"));
            }
            if !plugin.manifest.runtime_join_queries.is_empty() {
                runtime_joins.insert((id.clone(), version.clone()), plugin.manifest.runtime_join_queries.clone());
            }
            if !plugin.manifest.runtime_queries.is_empty() {
                runtime_queries.insert((id.clone(), version.clone()), plugin.manifest.runtime_queries.clone());
            }
            native.registry.register_plugin(plugin).await.map_err(|e| e.to_string())?;
            runtime
                .approve_curated_module(id.clone(), version.clone(), image.canonical_module_hash())
                .await;
            versions.insert(id, version);
        }
        Ok(Self { native: Arc::new(native), versions, runtime_queries, runtime_joins })
    }

    async fn register_release(&self, object_dir: PathBuf, release: &Release) -> Result<DuckLakeCatalogEngine, Error> {
        let catalog = blocking(move || {
            DuckLakeCatalogEngine::open_verified(&object_dir.join("ducklake"))
                .map_err(|e| format!("storage: {e}"))
        })
        .await?;
        let snapshot = &catalog.summary().latest_sealed;
        if release.manifest["snapshot_id"] != snapshot.snapshot_id
            || release.manifest["dataset_digest"] != snapshot.dataset_digest
            || release.manifest["catalog_id"].as_str() != Some(snapshot.catalog_id.as_str())
        {
            return Err(Error(
                "conflict: private release and sealed catalog disagree".into(),
            ));
        }
        self.native
            .local_runtime
            .as_ref()
            .expect("local bootstrap creates runtime")
            .register_immutable_catalog(catalog.clone())
            .await
            .map_err(run_error)?;
        Ok(catalog)
    }

    async fn saved_run(&self, id: &str) -> Result<RunRecord, Error> {
        let id = RunId::new(id).map_err(|e| Error(format!("invalid: {e}")))?;
        self.native
            .session_store
            .get_run(&id)
            .await
            .map_err(run_error)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetPrepare {
    version: u64,
    search_type: String,
    search_term: String,
    #[serde(default)]
    hla_filter: String,
    #[serde(default = "default_strip_points")]
    max_strip_points: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModulePrepare {
    version: u64,
    plugin_id: PluginId,
    params: serde_json::Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum Prepare {
    Module(ModulePrepare),
    Target(TargetPrepare),
}

impl Prepare {
    fn into_fields(self) -> (u64, PluginId, Value) {
        match self {
            Self::Module(request) => (request.version, request.plugin_id, Value::Object(request.params)),
            Self::Target(request) => (request.version, PluginId::from(TARGET_VALIDATION), json!({
                "search_type": request.search_type,
                "search_term": request.search_term.trim(),
                "hla_filter": request.hla_filter.trim(),
                "max_strip_points": request.max_strip_points,
            })),
        }
    }
}

fn default_strip_points() -> u64 {
    100
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Confirm {
    idempotency_key: String,
}

pub(super) fn run_error(error: CompassError) -> Error {
    let code = match &error {
        CompassError::Validation(_) => "invalid",
        CompassError::AccessDenied(_) => "forbidden",
        CompassError::NotFound(_) => "not_found",
        CompassError::IntegrityViolation(_) => "conflict",
        _ => "storage",
    };
    Error(format!("{code}: {error}"))
}

pub(super) fn release_relation(release: &Release) -> Result<Option<ScopeRelation>, Error> {
    release.manifest.get("scope_relation").map(|relation| {
        serde_json::from_value(relation.clone()).map_err(|_| Error("storage: invalid release scope relation".into()))
    }).transpose()
}

pub(super) fn access(
    app: &App,
    user: &User,
    id: &str,
    version: u64,
) -> Result<(Release, DatasetGrant, PathBuf), Error> {
    let directory = app.store.dataset_directory(id)?;
    let policy = query_access::load(&directory, user)?;
    if policy
        .as_ref()
        .is_some_and(|p| p.access == query_access::Access::Revoked)
    {
        return Err(Error("forbidden: dataset grant revoked".into()));
    }
    let state = if policy.is_some() {
        app.store.published_state(id)?
    } else if user.issuer == app.issuer {
        app.store.read_state(id, &user.org_id, &user.user_id)?
    } else {
        return Err(Error("forbidden: dataset grant required".into()));
    };
    let release = state.version(Some(version))?.clone();
    let scoped_relation = release_relation(&release)?;
    if scoped_relation.is_some() && policy.is_none() {
        return Err(Error("forbidden: explicit scoped grant required for this catalog".into()));
    }
    let reference = release.manifest["catalog_id"]
        .as_str()
        .ok_or("storage: published catalog ID missing")
        .map_err(str::to_owned)?;
    let catalog_id = CatalogId::new(reference)
        .map_err(|_| Error("storage: invalid published catalog ID".into()))?;
    let org =
        OrgId::new(&user.org_id).map_err(|_| Error("unauthorized: invalid organization".into()))?;
    let mut grant = DatasetGrant::compute_only_partner(org, catalog_id);
    let mut binding = json!([
        &user.org_id,
        &user.user_id,
        reference,
        release.version,
        &release.manifest["dataset_digest"]
    ]);
    if let Some(relation) = &scoped_relation {
        binding = json!([binding, id, relation]);
    }
    if let Some(policy) = policy {
        binding = json!([binding, &user.issuer, policy.epoch]);
        let column = scoped_relation.as_ref()
            .map(|relation| relation.parent_key.as_str()).unwrap_or("original_sample_number");
        grant.row_scope = Some(query_access::filter(&policy, column));
    }
    grant.grant_id = format!(
        "private-{}",
        hex::encode(Sha256::digest(binding.to_string()))
    );
    grant.label = format!("Private dataset {id}, version {}", release.version);
    let path = app.root.join(id).join(&release.object_dir);
    Ok((release, grant, path))
}

pub async fn prepare(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<Prepare>,
) -> Result<Json<RunQuote>, Error> {
    let user = app.query_user(&headers).await?;
    let (version, plugin_id, mut params) = request.into_fields();
    let (release, grant, path) = access(&app, &user, &id, version)?;
    if params.get("dataset_version").is_some() || params.get("cohort_filter").is_some() {
        return Err(Error("invalid: dataset version and snapshot are set by the private provider".into()));
    }
    let runs = configured(&app)?;
    let plugin_version = runs.versions.get(&plugin_id)
        .ok_or_else(|| Error("not_found: requested reviewed module is not configured".into()))?
        .clone();
    let joins = runs.runtime_joins.get(&(plugin_id.clone(), plugin_version.clone()));
    let relation = release_relation(&release)?;
    match (&relation, joins) {
        (None, None) => validate_reviewed_hla_capabilities(
            &plugin_id, &release.manifest["validation"]["capabilities"], &params,
        ).map_err(run_error)?,
        (Some(_), Some(_)) => {}
        _ => return Err(Error("invalid: this module's declared inputs do not apply to the dataset template".into())),
    }
    let catalog = runs.register_release(path, &release).await?;
    if let (Some(relation), Some(joins)) = (&relation, joins) {
        relation.scoped_ids(&grant, &catalog.summary().latest_sealed).map_err(run_error)?;
        for definition in joins.values() {
            relation.validate_query(definition, &catalog.summary().latest_sealed).map_err(run_error)?;
        }
    }
    params["dataset_version"] = json!(release.version);
    params["cohort_filter"] = json!({"snapshot_id": release.manifest["snapshot_id"]});
    runs.native
        .dispatcher
        .prepare_run(PrepareRunRequest {
            org_id: grant.org_id.clone(),
            session_id: None,
            plugin_id,
            plugin_version: Some(plugin_version),
            offer_id: None,
            params,
            dataset_grant: Some(grant),
        })
        .await
        .map(Json)
        .map_err(run_error)
}

pub async fn confirm(
    State(app): State<App>,
    Path((id, run_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<Confirm>,
) -> Result<Json<RunRecord>, Error> {
    let user = app.query_user(&headers).await?;
    let runs = configured(&app)?;
    let (record, release, path) = access_run(&app, runs, &user, &id, &run_id).await?;
    let catalog = runs.register_release(path, &release).await?;
    let module = (record.plugin_id.clone(), record.plugin_version.clone());
    let session = if let Some(joins) = runs.runtime_joins.get(&module) {
        let relation = release_relation(&release)?.ok_or_else(|| Error("conflict: scoped join requires a template relation".into()))?;
        let session = run_queries::session(&app, &user, &id, &release, catalog, &record, Vec::new())?;
        Some(session.with_scoped_joins(relation, joins.clone()))
    } else {
        runs.runtime_queries.get(&module).map(|queries| {
            run_queries::session(&app, &user, &id, &release, catalog, &record, queries.clone())
        }).transpose()?
    };
    let queries = session.map(|session| Arc::new(session) as Arc<dyn biocompass_oci_runner::LiveQueryResponder>);
    let runs = runs.clone();
    let result = tokio::spawn(async move {
        use std::sync::atomic::{AtomicBool, Ordering};
        let revoked = AtomicBool::new(false);
        let runtime = runs.native.local_runtime.as_ref().expect("native runtime");
        let running = runtime.confirm_run_with_queries(
            &record.run_id, &request.idempotency_key, Some(&revoked), queries,
        );
        tokio::pin!(running);
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(50));
        let grant = record.grant.as_ref().expect("ownership checked");
        let allowed = || access(&app, &user, &id,release.version).ok()
            .is_some_and(|(_,current,_)|query_access::supports(&current,grant));
        loop {
            tokio::select! {
                biased;
                _ = tick.tick(), if !revoked.load(Ordering::Acquire) => {
                    if !allowed() { revoked.store(true, Ordering::Release); }
                }
                result = &mut running => {
                    let mut result = result.map_err(run_error)?;
                    if !allowed() {
                        result.result_json=None;
                        result.artifacts.clear();
                        if result.status==RunStatus::Succeeded {
                            return Err(Error("forbidden: dataset access revoked; result withheld".into()));
                        }
                    }
                    return Ok(Json(result));
                }
            }
        }
    }).await.map_err(|_|Error("storage: governed run task failed".into()))?;
    result
}

pub async fn export(
    State(app): State<App>,
    Path((id, run_id, name)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    if name.is_empty() || name.starts_with('.') || !name.bytes().all(|ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, b'.' | b'-' | b'_')
    }) {
        return Err(Error("not_found: artifact unavailable".into()));
    }
    let user = app.query_user(&headers).await?;
    let runs = configured(&app)?;
    let (record, _, _) = access_run(&app, runs, &user, &id, &run_id).await?;
    let art = record
        .artifacts
        .iter()
        .find(|art| {
            art.name == name
                && record.status == RunStatus::Succeeded
                && record.receipt_verified
        })
        .ok_or_else(|| Error("not_found: artifact unavailable".into()))?;
    let data = runs
        .native
        .blob_store
        .get_bytes(&art.blob_key)
        .await
        .map_err(run_error)?;
    biocompass_node_runtime::verify_released_artifact(
        art,
        &hex::encode(Sha256::digest(&data)),
        data.len() as u64,
    )
    .map_err(run_error)?;
    access_run(&app, runs, &user, &id, &run_id).await?;
    let mut headers = HeaderMap::new();
    let media_type = if art.name.ends_with(".csv") {
        HeaderValue::from_static("text/csv; charset=utf-8")
    } else {
        HeaderValue::from_str(&art.media_type)
            .unwrap_or(HeaderValue::from_static("application/octet-stream"))
    };
    headers.insert(header::CONTENT_TYPE, media_type);
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename={name}"))
            .map_err(|_| Error("invalid: invalid artifact filename".into()))?,
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    Ok((headers, data).into_response())
}

async fn access_run(
    app: &App,
    runs: &PrivateRuns,
    user: &User,
    id: &str,
    run_id: &str,
) -> Result<(RunRecord, Release, PathBuf), Error> {
    let record = runs.saved_run(run_id).await?;
    let version = record.params["dataset_version"]
        .as_u64()
        .ok_or_else(|| Error("forbidden: run has no private dataset version".into()))?;
    let (release, grant, path) = access(app, user, id, version)?;
    if !record
        .grant
        .as_ref()
        .is_some_and(|pinned| query_access::supports(&grant, pinned))
    {
        return Err(Error(
            "forbidden: run does not belong to the authorized dataset user".into(),
        ));
    }
    Ok((record, release, path))
}

fn configured(app: &App) -> Result<&PrivateRuns, Error> {
    app.runs
        .as_ref()
        .ok_or_else(|| Error("issuer: private HLA modules are not configured".into()))
}
