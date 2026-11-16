//! Shared application state & zero-source-tree bootstrap for `biocompass`.

use biocompass_agent_core::{AgentConfig, AgentRuntime};
use biocompass_core_traits::{
    BlobStore, CatalogId, CompassError, CompassResult, CreditAmountAct, DatasetGrant,
    LocalFsBlobStore, OciRegistryClient, OrgId, PaymentGateway, SessionStore, SqliteActLedger,
    SqliteSessionStore, StandardOciPolicyVerifier,
};
use biocompass_data_engine::{DuckDbCatalogEngine, seed_demo_hla_catalog};
use biocompass_node_runtime::{InProcessNodeRuntime, NodeKeypairSet, NodeRuntimeDeps};
use biocompass_oci_runner::{OciContainerRunner, OciDistributionClient};
use biocompass_plugin_spec::LoadedPlugin;
use rust_embed::RustEmbed;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// Shared state injected into the Axum HTTP/SSE router.
#[derive(Clone)]
pub struct AppState {
    pub mode: DeploymentMode,
    pub default_org_id: OrgId,
    pub data_dir: PathBuf,
    pub user_plugins_dir: PathBuf,
    pub allow_local_dev_tags: bool,
    pub session_store: Arc<dyn SessionStore>,
    pub blob_store: Arc<dyn BlobStore>,
    pub payment_gateway: Arc<dyn PaymentGateway>,
    pub runtime: Arc<InProcessNodeRuntime>,
    pub agent: Arc<AgentRuntime>,
    pub default_catalog_id: CatalogId,
    pub container_engine_label: String,
}

impl AppState {
    /// Bootstrap stores, seed verified DuckDB Parquet catalog, extract bundled plugins into `data_dir`
    /// (so standalone binaries run anywhere without the git repo), and bind the container engine.
    pub async fn bootstrap(
        data_dir: impl AsRef<Path>,
        mode: DeploymentMode,
        allow_local_dev_tags: bool,
    ) -> CompassResult<Self> {
        let root = data_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)
            .map_err(|e| CompassError::Storage(format!("create data_dir {}: {e}", root.display())))?;

        let default_org_id =
            OrgId::new("org-local-scientist").map_err(CompassError::Validation)?;
        let keys = NodeKeypairSet::load_or_generate(root.join("keys"))?;

        let session_store: Arc<dyn SessionStore> =
            Arc::new(SqliteSessionStore::open(root.join("sessions.sqlite"))?);
        let blob_store: Arc<dyn BlobStore> = Arc::new(LocalFsBlobStore::new(root.join("store"))?);
        let payment_gateway: Arc<dyn PaymentGateway> = Arc::new(SqliteActLedger::open(
            root.join("ledger.sqlite"),
            CreditAmountAct::from_whole_act(250),
        )?);

        // Ensure genesis balance row is initialized on first boot.
        let _ = payment_gateway.available_balance(&default_org_id).await?;

        let oci_registry: Arc<dyn OciRegistryClient> = if allow_local_dev_tags {
            Arc::new(StandardOciPolicyVerifier::workstation_dev())
        } else {
            Arc::new(OciDistributionClient::new(true)?)
        };

        let oci_runner = Arc::new(OciContainerRunner::detect().await?);
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
            oci_runner,
            stage_root_dir: root.join("stages"),
            allow_local_dev_tags,
        })?);

        // Seed or verify the reference HLA Immunopeptidomics Parquet catalog.
        let catalog_dir = root.join("catalogs").join("hla-immunopeptidomics-v1");
        if !catalog_dir.join("catalog.json").is_file() {
            seed_demo_hla_catalog(&catalog_dir)?;
        }
        let catalog = DuckDbCatalogEngine::open_verified(&catalog_dir)?;
        let default_catalog_id = catalog.manifest().id.clone();
        runtime.register_catalog(catalog).await?;

        // Extract embedded first-party plugins (`plugin.toml`, `SKILL.md`, `ui/dist/*`) into `data_dir/bundled-plugins`.
        let bundled_dir = root.join("bundled-plugins");
        extract_bundled_plugins(&bundled_dir)?;
        load_plugins_from_parent(&bundled_dir, allow_local_dev_tags, true, &runtime).await?;

        // Load user-created or scaffolded plugins from `data_dir/plugins`.
        // Unpinned `:dev` skeletons are skipped with a warning unless `--allow-local-dev-tags` is set.
        let user_plugins_dir = root.join("plugins");
        std::fs::create_dir_all(&user_plugins_dir)
            .map_err(|e| CompassError::Storage(format!("create user plugins dir: {e}")))?;
        load_plugins_from_parent(&user_plugins_dir, allow_local_dev_tags, false, &runtime).await?;

        let agent_cfg = AgentConfig {
            scaffold_root_dir: user_plugins_dir.clone(),
            ..AgentConfig::default()
        };
        let agent = Arc::new(AgentRuntime::new(
            agent_cfg,
            session_store.clone(),
            blob_store.clone(),
            runtime.clone(),
        )?);

        // Ensure at least one default session exists for immediate UI/API use.
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
            mode,
            default_org_id,
            data_dir: root,
            user_plugins_dir,
            allow_local_dev_tags,
            session_store,
            blob_store,
            payment_gateway,
            runtime,
            agent,
            default_catalog_id,
            container_engine_label,
        })
    }

    /// Return the canonical `DatasetGrant` presets available for `default_org_id`.
    pub fn available_grants(&self) -> Vec<DatasetGrant> {
        vec![
            DatasetGrant::full_access(self.default_org_id.clone(), self.default_catalog_id.clone()),
            DatasetGrant::restricted_partner_demo(
                self.default_org_id.clone(),
                self.default_catalog_id.clone(),
            ),
        ]
    }

    /// Resolve a `grant_id` (or `"full"` / `"partner"` shorthand) into a `DatasetGrant`.
    pub fn resolve_grant(&self, grant_id: Option<&str>) -> CompassResult<Option<DatasetGrant>> {
        let Some(raw) = grant_id else {
            return Ok(None);
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
            return Ok(None);
        }
        let grants = self.available_grants();
        if trimmed.eq_ignore_ascii_case("full") {
            return Ok(grants.into_iter().next());
        }
        if trimmed.eq_ignore_ascii_case("partner") {
            return Ok(grants.into_iter().nth(1));
        }
        grants
            .into_iter()
            .find(|g| g.grant_id == trimmed)
            .map(Some)
            .ok_or_else(|| CompassError::NotFound(format!("unknown grant_id '{trimmed}'")))
    }
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
    runtime: &InProcessNodeRuntime,
) -> CompassResult<()> {
    if !parent.is_dir() {
        return Ok(());
    }
    let entries = std::fs::read_dir(parent)
        .map_err(|e| CompassError::Storage(format!("read {}: {e}", parent.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| CompassError::Storage(format!("dir entry: {e}")))?;
        let dir = entry.path();
        if dir.is_dir() && dir.join("plugin.toml").is_file() {
            match LoadedPlugin::load_from_dir(&dir, allow_local_dev_tags) {
                Ok(plugin) => {
                    if let Err(err) = runtime.register_plugin(plugin).await {
                        if fail_on_invalid {
                            return Err(err);
                        }
                        tracing::warn!("Skipping draft plugin {}: {}", dir.display(), err);
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
