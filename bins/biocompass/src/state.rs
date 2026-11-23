//! Shared application state & zero-source-tree bootstrap for `biocompass` (`local` and `cloud-web`).

use crate::cloud_client::{CompassHubClient, CompassHubConfig};
use crate::llm_backends::{CachedProviderControls, LlmControlsCache};
use async_trait::async_trait;
use biocompass_agent_core::llm::LlmProviderConfig;
use biocompass_agent_core::{AgentConfig, AgentRuntime};
use biocompass_core_traits::{
    BlobStore, CatalogId, CompassError, CompassResult, CreditAmountAct, DatasetGrant, GrantLevel,
    LocalFsBlobStore, NodeId, OciImageRef, OciRegistryClient, OrgId, PaymentGateway, PluginId,
    QueryAst, RunDispatcher, SessionStore, SqliteActLedger, SqliteSessionStore,
    StandardOciPolicyVerifier,
};
use biocompass_data_engine::{
    CatalogSummary, DuckDbCatalogEngine, QueryTableResult, seed_demo_hla_catalog,
};
use biocompass_node_runtime::{InProcessNodeRuntime, NodeKeypairSet, NodeRuntimeDeps};
use biocompass_oci_runner::{OciContainerRunner, OciDistributionClient};
use biocompass_plugin_spec::LoadedPlugin;
use rust_embed::RustEmbed;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// An imported distribution archive is available offline under the actual engine's immutable repository digest.
struct WorkstationImageVerifier {
    runner: Arc<OciContainerRunner>,
    registry: OciDistributionClient,
}

#[async_trait]
impl OciRegistryClient for WorkstationImageVerifier {
    async fn verify_image(&self, image_raw: &str) -> CompassResult<OciImageRef> {
        let pin = OciImageRef::parse(image_raw, false)?;
        if self.runner.has_verified_local_digest(&pin.raw).await {
            return Ok(pin);
        }
        self.registry.verify_image(&pin.raw).await
    }
}

#[derive(RustEmbed)]
#[folder = "../../plugins/"]
#[exclude = "*/ui/node_modules/*"]
#[exclude = "*/ui/src/*"]
#[exclude = "*/__pycache__/*"]
pub struct BundledPluginsAssets;

/// Operating deployment profile selected at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentMode {
    Local,
    CloudWeb,
}

impl DeploymentMode {
    pub fn parse(raw: &str) -> CompassResult<Self> {
        match raw.trim() {
            "local" => Ok(Self::Local),
            "cloud-web" => Ok(Self::CloudWeb),
            other => Err(CompassError::Validation(format!(
                "invalid --mode '{other}'; expected 'local' or 'cloud-web'"
            ))),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::CloudWeb => "cloud-web",
        }
    }
}

/// Transport-agnostic workspace catalog, plugin, and grant query port (`InProcessNodeRuntime` locally; `CompassHubClient` in Cloud).
#[async_trait]
pub trait WorkspaceRegistryPort: Send + Sync {
    fn node_id(&self) -> NodeId;
    fn provider_id(&self) -> String;
    fn requester_id(&self) -> String;
    async fn list_catalog_manifests(&self) -> CompassResult<Vec<CatalogSummary>>;
    async fn query_catalog(
        &self,
        id: &CatalogId,
        grant: &DatasetGrant,
        query: &QueryAst,
    ) -> CompassResult<QueryTableResult>;
    async fn rollback_catalog_snapshot(
        &self,
        id: &CatalogId,
        target_snapshot_id: u64,
    ) -> CompassResult<CatalogSummary>;
    async fn set_catalog_snapshot_pin(
        &self,
        id: &CatalogId,
        snapshot_id: u64,
        pinned: bool,
    ) -> CompassResult<CatalogSummary>;
    async fn list_plugins(&self) -> CompassResult<Vec<LoadedPlugin>>;
    async fn get_plugin(&self, id: &PluginId) -> CompassResult<LoadedPlugin>;
    async fn get_plugin_version(&self, id: &PluginId, version: Option<&str>) -> CompassResult<LoadedPlugin> {
        let plugin = self.get_plugin(id).await?;
        if let Some(version) = version
            && plugin.manifest.version != version
        {
            return Err(CompassError::NotFound(format!("plugin '{id}' version '{version}' is unavailable")));
        }
        Ok(plugin)
    }
    async fn register_plugin(&self, plugin: LoadedPlugin) -> CompassResult<()>;
    async fn list_grants(
        &self,
        org_id: &OrgId,
        default_catalog_id: &CatalogId,
    ) -> CompassResult<Vec<DatasetGrant>>;
}

#[async_trait]
impl WorkspaceRegistryPort for InProcessNodeRuntime {
    fn node_id(&self) -> NodeId {
        InProcessNodeRuntime::node_id(self).clone()
    }

    fn provider_id(&self) -> String {
        InProcessNodeRuntime::provider_id(self)
    }

    fn requester_id(&self) -> String {
        InProcessNodeRuntime::requester_id(self)
    }

    async fn list_catalog_manifests(&self) -> CompassResult<Vec<CatalogSummary>> {
        Ok(InProcessNodeRuntime::list_catalogs(self)
            .await
            .into_iter()
            .map(|c| c.manifest().clone())
            .collect())
    }

    async fn query_catalog(
        &self,
        id: &CatalogId,
        grant: &DatasetGrant,
        query: &QueryAst,
    ) -> CompassResult<QueryTableResult> {
        let cat = InProcessNodeRuntime::get_catalog(self, id).await?;
        if query.additional_sources.is_empty() {
            return cat.query(grant, query);
        }
        let mut attached_owned = Vec::with_capacity(query.additional_sources.len());
        for pin in &query.additional_sources {
            let other_cat = InProcessNodeRuntime::get_catalog(self, &pin.catalog_id).await?;
            let candidates = self.list_grants(&grant.org_id, &pin.catalog_id).await?;
            let other_grant = candidates
                .into_iter()
                .find(|g| g.grant_id == pin.grant_id || (pin.grant_id == "full" && g.level == GrantLevel::Rows))
                .ok_or_else(|| {
                    CompassError::AccessDenied(format!(
                        "grant '{}' not found for attached catalog '{}'",
                        pin.grant_id, pin.catalog_id
                    ))
                })?;
            attached_owned.push((other_cat, other_grant, pin.snapshot_id));
        }
        let attached_refs: Vec<_> = attached_owned
            .iter()
            .map(|(c, g, sid)| (c, g, *sid))
            .collect();
        cat.query_multi(grant, query, &attached_refs)
    }

    async fn rollback_catalog_snapshot(
        &self,
        id: &CatalogId,
        target_snapshot_id: u64,
    ) -> CompassResult<CatalogSummary> {
        self.mutate_catalog(id, |c| {
            let _ = c.rollback_to_snapshot(target_snapshot_id)?;
            Ok(c.manifest().clone())
        })
        .await
    }

    async fn set_catalog_snapshot_pin(
        &self,
        id: &CatalogId,
        snapshot_id: u64,
        pinned: bool,
    ) -> CompassResult<CatalogSummary> {
        self.mutate_catalog(id, |c| {
            if pinned {
                c.pin_snapshot(snapshot_id)?;
            } else {
                c.unpin_snapshot(snapshot_id)?;
            }
            Ok(c.manifest().clone())
        })
        .await
    }

    async fn list_plugins(&self) -> CompassResult<Vec<LoadedPlugin>> {
        Ok(InProcessNodeRuntime::list_plugins(self).await)
    }

    async fn get_plugin(&self, id: &PluginId) -> CompassResult<LoadedPlugin> {
        InProcessNodeRuntime::get_plugin(self, id).await
    }

    async fn get_plugin_version(&self, id: &PluginId, version: Option<&str>) -> CompassResult<LoadedPlugin> {
        InProcessNodeRuntime::get_plugin_version(self, id, version).await
    }

    async fn register_plugin(&self, plugin: LoadedPlugin) -> CompassResult<()> {
        InProcessNodeRuntime::register_plugin(self, plugin).await
    }

    async fn list_grants(
        &self,
        org_id: &OrgId,
        default_catalog_id: &CatalogId,
    ) -> CompassResult<Vec<DatasetGrant>> {
        let mut grants = vec![
            DatasetGrant::full_access(org_id.clone(), default_catalog_id.clone()),
            DatasetGrant::scoped_rows_partner(org_id.clone(), default_catalog_id.clone()),
            DatasetGrant::compute_only_partner(org_id.clone(), default_catalog_id.clone()),
        ];
        for cat in InProcessNodeRuntime::list_catalogs(self).await {
            let cid = cat.manifest().id.clone();
            if &cid != default_catalog_id {
                grants.push(DatasetGrant::full_access(org_id.clone(), cid.clone()));
                grants.push(DatasetGrant::compute_only_partner(org_id.clone(), cid));
            }
        }
        Ok(grants)
    }
}

/// Shared state injected into the Axum HTTP/SSE router.
#[derive(Clone)]
pub struct AppState {
    pub(crate) local_owner: Option<Arc<crate::native_import::operator::OperatorAccess>>,
    pub mode: DeploymentMode,
    pub default_org_id: OrgId,
    pub data_dir: PathBuf,
    pub user_plugins_dir: PathBuf,
    pub allow_local_dev_tags: bool,
    pub session_store: Arc<dyn SessionStore>,
    pub local_sqlite: Option<Arc<SqliteSessionStore>>,
    pub local_runtime: Option<Arc<InProcessNodeRuntime>>,
    pub blob_store: Arc<dyn BlobStore>,
    pub payment_gateway: Arc<dyn PaymentGateway>,
    pub dispatcher: Arc<dyn RunDispatcher>,
    pub registry: Arc<dyn WorkspaceRegistryPort>,
    pub installed_workspace: Option<Arc<crate::workspace::InstalledWorkspace>>,
    pub hub_client: Option<Arc<CompassHubClient>>,
    pub agent: Arc<AgentRuntime>,
    pub default_catalog_id: CatalogId,
    pub container_engine_label: String,
    pub oci_runner: Option<Arc<OciContainerRunner>>,
    pub llm_controls_cache: LlmControlsCache,
}

impl AppState {
    /// Called only after the personal workbench has bound its listening socket.
    pub fn enable_local_owner(&mut self, addr: std::net::SocketAddr) -> CompassResult<Option<String>> {
        if self.mode != DeploymentMode::Local || !addr.ip().is_loopback() {
            return Ok(None);
        }
        let (access, link) = crate::native_import::operator::OperatorAccess::launch(addr, &self.data_dir)?;
        self.local_owner = Some(access);
        Ok(Some(link))
    }

    /// Bootstrap stores and execution bindings for either `DeploymentMode::Local` or `DeploymentMode::CloudWeb`.
    pub async fn bootstrap(
        data_dir: impl AsRef<Path>,
        mode: DeploymentMode,
        allow_local_dev_tags: bool,
    ) -> CompassResult<Self> {
        match mode {
            DeploymentMode::Local => Self::bootstrap_local(data_dir, allow_local_dev_tags).await,
            DeploymentMode::CloudWeb => {
                Self::bootstrap_cloud_web(data_dir, allow_local_dev_tags).await
            }
        }
    }

    async fn bootstrap_local(
        data_dir: impl AsRef<Path>,
        allow_local_dev_tags: bool,
    ) -> CompassResult<Self> {
        let root = data_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)
            .map_err(|e| CompassError::Storage(format!("create data_dir {}: {e}", root.display())))?;

        let default_org_id =
            OrgId::new("org-local-scientist").map_err(CompassError::Validation)?;
        let keys = NodeKeypairSet::load_or_generate(root.join("keys"))?;

        let sqlite_store = Arc::new(SqliteSessionStore::open(root.join("sessions.sqlite"))?);
        let session_store: Arc<dyn SessionStore> = sqlite_store.clone();
        let blob_store: Arc<dyn BlobStore> = Arc::new(LocalFsBlobStore::new(root.join("store"))?);
        let payment_gateway: Arc<dyn PaymentGateway> = Arc::new(SqliteActLedger::open(
            root.join("ledger.sqlite"),
            CreditAmountAct::from_whole_act(250),
        )?);

        let _ = payment_gateway.available_balance(&default_org_id).await?;

        let oci_runner = Arc::new(OciContainerRunner::detect().await?);
        let oci_registry: Arc<dyn OciRegistryClient> = if allow_local_dev_tags {
            Arc::new(StandardOciPolicyVerifier::workstation_dev())
        } else {
            Arc::new(WorkstationImageVerifier { runner: Arc::clone(&oci_runner), registry: OciDistributionClient::new(true)? })
        };
        let container_engine_label = format!(
            "{} {} (rootless={})",
            oci_runner.engine().kind.as_str(),
            oci_runner.engine().server_version,
            oci_runner.engine().rootless
        );

        let runtime = Arc::new(InProcessNodeRuntime::new(NodeRuntimeDeps {
            keys,
            session_store: session_store.clone(),
            blob_store: blob_store.clone(),
            oci_registry,
            payment_gateway: payment_gateway.clone(),
            oci_runner: oci_runner.clone(),
            stage_root_dir: root.join("stages"),
            allow_local_dev_tags,
        })?);

        let catalogs_root = root.join("catalogs");
        let catalog_dir = catalogs_root.join("hla-immunopeptidomics-v1");
        if !catalog_dir.join("catalog.json").is_file() || !catalog_dir.join("snapshots").is_dir() {
            let _ = std::fs::remove_dir_all(&catalog_dir);
            seed_demo_hla_catalog(&catalog_dir)?;
        }
        let catalog = DuckDbCatalogEngine::open_verified(&catalog_dir)?;
        let default_catalog_id = catalog.manifest().id.clone();
        runtime.register_catalog(catalog).await?;

        // Load any user-imported local catalogs persisted under `<data_dir>/catalogs/**`.
        load_additional_local_catalogs(&catalogs_root, &catalog_dir, &runtime).await;
        crate::native_import::restore(&root, &runtime).await?;

        let bundled_dir = root.join("bundled-plugins");
        extract_bundled_plugins(&bundled_dir)?;
        load_plugins_from_parent(&bundled_dir, allow_local_dev_tags, true, true, None, &runtime).await?;

        crate::package_install::restore(&root, &runtime).await?;

        let user_plugins_dir = root.join("plugins");
        std::fs::create_dir_all(&user_plugins_dir)
            .map_err(|e| CompassError::Storage(format!("create user plugins dir: {e}")))?;
        load_plugins_from_parent(&user_plugins_dir, allow_local_dev_tags, false, false, Some(&root), &runtime).await?;

        if let Err(error) = runtime.recover_abandoned_containers().await {
            tracing::warn!(%error, "native container cleanup unconfirmed; will retry on next startup");
        }
        for run_id in sqlite_store.recovery_candidate_run_ids().await? {
            if let Err(error) = runtime.recover_interrupted_run(&run_id).await {
                tracing::warn!(%run_id, %error, "local run recovery failed; will retry on next startup");
            }
        }

        let saved_llm = load_saved_llm_config(&root);
        let default_agent_cfg = AgentConfig::default();
        let agent_cfg = AgentConfig {
            llm: saved_llm.or(default_agent_cfg.llm),
            scaffold_root_dir: user_plugins_dir.clone(),
            ..default_agent_cfg
        };
        let agent = Arc::new(AgentRuntime::new(
            agent_cfg,
            session_store.clone(),
            blob_store.clone(),
            runtime.clone(),
        )?);

        let existing_sessions = session_store.list_sessions(&default_org_id).await?;
        if existing_sessions.is_empty() {
            let full_grant =
                DatasetGrant::full_access(default_org_id.clone(), default_catalog_id.clone());
            session_store
                .create_session(
                    &default_org_id,
                    "HLA Class I Immunopeptidomics Discovery",
                    &full_grant.grant_id,
                )
                .await?;
        }

        Ok(Self {
            mode: DeploymentMode::Local,
            default_org_id,
            data_dir: root,
            user_plugins_dir,
            allow_local_dev_tags,
            session_store,
            local_sqlite: Some(sqlite_store),
            local_runtime: Some(runtime.clone()),
            blob_store,
            payment_gateway,
            dispatcher: runtime.clone(),
            registry: runtime,
            local_owner: None,
            installed_workspace: None,
            hub_client: None,
            agent,
            default_catalog_id,
            container_engine_label,
            oci_runner: Some(oci_runner),
            llm_controls_cache: Arc::new(RwLock::new(HashMap::<String, CachedProviderControls>::new())),
        })
    }

    async fn bootstrap_cloud_web(
        data_dir: impl AsRef<Path>,
        allow_local_dev_tags: bool,
    ) -> CompassResult<Self> {
        let root = data_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)
            .map_err(|e| CompassError::Storage(format!("create data_dir {}: {e}", root.display())))?;

        let bundled_dir = root.join("bundled-plugins");
        extract_bundled_plugins(&bundled_dir)?;
        let mut bundled_ui_roots = collect_bundled_ui_roots(&bundled_dir);

        let user_plugins_dir = root.join("plugins");
        std::fs::create_dir_all(&user_plugins_dir)
            .map_err(|e| CompassError::Storage(format!("create user plugins dir: {e}")))?;
        bundled_ui_roots.extend(collect_bundled_ui_roots(&user_plugins_dir));

        let hub_cfg = CompassHubConfig::from_env(bundled_dir)?;
        let (hub_client, default_org_id, container_engine_label) =
            CompassHubClient::connect(hub_cfg, bundled_ui_roots).await?;
        let hub = Arc::new(hub_client);

        let default_catalog_id =
            CatalogId::new("alithea/hla-atlas-v2026.1").map_err(CompassError::Validation)?;

        let session_store: Arc<dyn SessionStore> = hub.clone();
        let blob_store: Arc<dyn BlobStore> = hub.clone();
        let payment_gateway: Arc<dyn PaymentGateway> = hub.clone();
        let dispatcher: Arc<dyn RunDispatcher> = hub.clone();
        let registry: Arc<dyn WorkspaceRegistryPort> = hub.clone();

        let default_agent_cfg = AgentConfig::default();
        let agent_cfg = AgentConfig {
            llm: default_agent_cfg.llm.filter(|c| !c.is_local_cli()),
            scaffold_root_dir: user_plugins_dir.clone(),
            ..default_agent_cfg
        };
        let agent = Arc::new(AgentRuntime::new(
            agent_cfg,
            session_store.clone(),
            blob_store.clone(),
            hub.clone(),
        )?);

        Ok(Self {
            mode: DeploymentMode::CloudWeb,
            default_org_id,
            data_dir: root,
            user_plugins_dir,
            allow_local_dev_tags,
            session_store,
            local_sqlite: None,
            local_runtime: None,
            blob_store,
            payment_gateway,
            dispatcher,
            registry,
            local_owner: None,
            installed_workspace: None,
            hub_client: Some(hub),
            agent,
            default_catalog_id,
            container_engine_label,
            oci_runner: None,
            llm_controls_cache: Arc::new(RwLock::new(HashMap::<String, CachedProviderControls>::new())),
        })
    }

    /// Return the `DatasetGrant` presets available for the caller (or `default_org_id` in local mode).
    pub async fn available_grants(&self) -> CompassResult<Vec<DatasetGrant>> {
        self.registry
            .list_grants(&self.default_org_id, &self.default_catalog_id)
            .await
    }

    /// Resolve a `grant_id` (or `"full"` / `"partner"` shorthand) into a `DatasetGrant`.
    pub async fn resolve_grant(&self, grant_id: Option<&str>) -> CompassResult<Option<DatasetGrant>> {
        let Some(raw) = grant_id else {
            return Ok(None);
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
            return Ok(None);
        }
        let grants = self.available_grants().await?;
        if trimmed.eq_ignore_ascii_case("full") {
            return Ok(grants.into_iter().next());
        }
        if trimmed.eq_ignore_ascii_case("partner") {
            return Ok(grants.into_iter().nth(1));
        }
        if trimmed.eq_ignore_ascii_case("compute_only") {
            return Ok(grants
                .iter()
                .find(|g| g.level == GrantLevel::ComputeOnly)
                .cloned());
        }
        grants
            .into_iter()
            .find(|g| g.grant_id == trimmed)
            .map(Some)
            .ok_or_else(|| match self.mode {
                DeploymentMode::CloudWeb => CompassError::AccessDenied(format!(
                    "FORBIDDEN: grant_id '{trimmed}' is not accessible to this organization"
                )),
                DeploymentMode::Local => {
                    CompassError::NotFound(format!("unknown grant_id '{trimmed}'"))
                }
            })
    }
}

fn collect_bundled_ui_roots(bundled_dir: &Path) -> HashMap<PluginId, PathBuf> {
    let mut map = HashMap::new();
    let Ok(entries) = std::fs::read_dir(bundled_dir) else {
        return map;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if dir.is_dir()
            && dir.join("plugin.toml").is_file()
            && let Ok(loaded) = LoadedPlugin::load_from_dir(&dir, true)
        {
            map.insert(loaded.manifest.id, dir);
        }
    }
    map
}

fn extract_bundled_plugins(dest_root: &Path) -> CompassResult<()> {
    std::fs::create_dir_all(dest_root)
        .map_err(|e| CompassError::Storage(format!("create bundled plugins dir: {e}")))?;
    for rel in BundledPluginsAssets::iter() {
        let rel_str = rel.as_ref();
        if rel_str.contains("..") {
            continue;
        }
        if let Some(asset) = BundledPluginsAssets::get(rel_str) {
            let out_path = dest_root.join(rel_str);
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    CompassError::Storage(format!("create {}: {e}", parent.display()))
                })?;
            }
            std::fs::write(&out_path, asset.data.as_ref()).map_err(|e| {
                CompassError::Storage(format!("write bundled {}: {e}", out_path.display()))
            })?;
        }
    }
    Ok(())
}

async fn load_plugins_from_parent(
    parent: &Path,
    allow_local_dev_tags: bool,
    fail_on_invalid: bool,
    approve_curated: bool,
    workspace_root: Option<&Path>,
    runtime: &InProcessNodeRuntime,
) -> CompassResult<()> {
    if !parent.is_dir() {
        return Ok(());
    }
    let entries = std::fs::read_dir(parent)
        .map_err(|e| CompassError::Storage(format!("read {}: {e}", parent.display())))?;
    // Registration order selects the default release. Filesystem enumeration order is unspecified.
    let mut dirs = entries
        .map(|e| e.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| CompassError::Storage(format!("plugin directory entry: {e}")))?;
    dirs.sort();
    for dir in dirs {
        if dir.is_dir() && dir.join("plugin.toml").is_file() {
            match LoadedPlugin::load_from_dir(&dir, allow_local_dev_tags) {
                Ok(plugin) => {
                    let pid = plugin.manifest.id.clone();
                    let ver = plugin.manifest.version.clone();
                    if let Some(root) = workspace_root
                        && crate::package_install::removal::is_detached(root, &pid, &ver)?
                    {
                        continue;
                    }
                    let img_raw = plugin.manifest.image.clone();
                    if let Err(err) = runtime.register_plugin(plugin).await {
                        if fail_on_invalid {
                            return Err(err);
                        }
                        tracing::warn!("Skipping draft plugin {}: {}", dir.display(), err);
                    } else if approve_curated
                        && let Ok(parsed_img) = OciImageRef::parse(&img_raw, true)
                    {
                        runtime
                            .approve_curated_module(pid, ver, parsed_img.canonical_module_hash())
                            .await;
                    }
                }
                Err(err) => {
                    if fail_on_invalid {
                        return Err(err);
                    }
                    tracing::warn!("Skipping plugin directory {}: {}", dir.display(), err);
                }
            }
        }
    }
    Ok(())
}

/// Filename under `data_dir` used in `DeploymentMode::Local` to persist UI-configured LLM settings.
const LOCAL_LLM_CONFIG_FILE: &str = "llm.json";

/// Load persisted local LLM configuration from `<data_dir>/llm.json` if present and valid.
pub fn load_saved_llm_config(data_dir: &Path) -> Option<LlmProviderConfig> {
    let path = data_dir.join(LOCAL_LLM_CONFIG_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return None,
    };
    match serde_json::from_str::<LlmProviderConfig>(&raw) {
        Ok(cfg) => Some(cfg),
        Err(err) => {
            tracing::warn!("Ignoring malformed {}: {}", path.display(), err);
            None
        }
    }
}

/// Persist (with `0600` permissions on Unix) or remove `<data_dir>/llm.json`.
pub fn save_saved_llm_config(
    data_dir: &Path,
    cfg: Option<&LlmProviderConfig>,
) -> CompassResult<()> {
    let path = data_dir.join(LOCAL_LLM_CONFIG_FILE);
    match cfg {
        None => {
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| {
                    CompassError::Storage(format!("remove {}: {e}", path.display()))
                })?;
            }
            Ok(())
        }
        Some(cfg) => {
            let encoded = serde_json::to_string_pretty(cfg)
                .map_err(|e| CompassError::Internal(format!("encode llm config: {e}")))?;
            std::fs::write(&path, encoded).map_err(|e| {
                CompassError::Storage(format!("write {}: {e}", path.display()))
            })?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let perms = std::fs::Permissions::from_mode(0o600);
                    std::fs::set_permissions(&path, perms).map_err(|e| {
                        CompassError::Storage(format!("chmod {}: {e}", path.display()))
                    })?;
                }
            Ok(())
        }
    }
}

/// Resolve the local filesystem directory for a `CatalogId` inside `<data_dir>/catalogs/`.
pub fn local_catalog_dir(data_dir: &Path, catalog_id: &CatalogId) -> PathBuf {
    let catalogs_root = data_dir.join("catalogs");
    if catalog_id.as_str() == "alithea/hla-immunopeptidomics-v1" {
        let legacy = catalogs_root.join("hla-immunopeptidomics-v1");
        if legacy.join("catalog.json").is_file() {
            return legacy;
        }
    }
    let (ns, name) = catalog_id
        .as_str()
        .split_once('/')
        .unwrap_or(("local", catalog_id.as_str()));
    catalogs_root.join(ns).join(name)
}

async fn load_additional_local_catalogs(
    catalogs_root: &Path,
    skip_dir: &Path,
    runtime: &InProcessNodeRuntime,
) {
    let Ok(level1) = std::fs::read_dir(catalogs_root) else {
        return;
    };
    let mut candidate_dirs = Vec::new();
    for e1 in level1.flatten() {
        let p1 = e1.path();
        if !p1.is_dir() || p1 == skip_dir {
            continue;
        }
        if p1.join("catalog.json").is_file() && p1.join("snapshots").is_dir() {
            candidate_dirs.push(p1);
            continue;
        }
        if let Ok(level2) = std::fs::read_dir(&p1) {
            for e2 in level2.flatten() {
                let p2 = e2.path();
                if p2.is_dir()
                    && p2 != skip_dir
                    && p2.join("catalog.json").is_file()
                    && p2.join("snapshots").is_dir()
                {
                    candidate_dirs.push(p2);
                }
            }
        }
    }
    candidate_dirs.sort();
    for dir in candidate_dirs {
        match DuckDbCatalogEngine::open_verified(&dir) {
            Ok(eng) => {
                if let Err(err) = runtime.register_catalog(eng).await {
                    tracing::warn!(dir = %dir.display(), error = %err, "skipping local catalog");
                }
            }
            Err(err) => {
                tracing::warn!(dir = %dir.display(), error = %err, "failed to verify local catalog");
            }
        }
    }
}
