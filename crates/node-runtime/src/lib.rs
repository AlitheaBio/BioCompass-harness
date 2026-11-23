//! `biocompass-node-runtime`: Two-step `Prepare -> Confirm` Froglet v1 execution node runtime.
//!
//! Orchestrates JSON Schema input validation, OCI Distribution v1.1 manifest verification,
//! double-entry ACT credit reservation/settlement/refund, DuckDB Grant-masked `/in/data/*.parquet`
//! materialization, hardened `OciContainerRunner` execution, `BlobStore` stage-out, and
//! RFC 8785 JCS + `secp256k1` BIP340 Schnorr 5-artifact (`Descriptor -> Offer -> Quote -> Deal -> Receipt`)
//! chain signing & verification via `froglet-protocol` and `froglet-verify`.

mod native_recovery;
mod recovered_result;
mod released_result;
mod run_lock;
mod target_validation;
pub use released_result::{
    verify_released_artifact, verify_released_result, verify_released_result_commitment,
};
mod live_query;
pub use live_query::{LiveQueryAuthority, LiveQueryBinding, LiveQuerySession};
pub use target_validation::{
    NamedReadPage, RuntimeReadParams, direct_named_read_page, is_runtime_read, named_read as private_named_read,
    runtime_named_read, runtime_named_read_on_catalog, sample_scope as private_sample_scope,
    reviewed_hla_input_tables,
};

use async_trait::async_trait;
use biocompass_core_traits::{
    ArtifactRef, BlobStore, CatalogId, CompassError, CompassResult, DatasetGrant, FilterExpr, GrantLevel,
    NodeId, OciImageRef, OciRegistryClient, PaymentGateway, PluginId, PrepareRunRequest, QueryAst,
    RunDispatcher, RunId, RunLogEvent, RunQuote, RunRecord, RunStatus, SessionEventKind,
    SessionStore,
};
use biocompass_data_engine::{DuckDbCatalogEngine, SealedSnapshot, StagingMode};
use biocompass_oci_runner::{ContainerRunSpec, OciContainerRunner, prepare_stage_permissions};
use biocompass_plugin_spec::{
    LoadedPlugin, PluginExecutionPlane,
    reviewed_hla_input_contract,
};
use chrono::{TimeZone, Utc};
use froglet_protocol::ExecutionRuntime;
use froglet_protocol::canonical_json;
use froglet_protocol::crypto::{
    self, NodeSigningKey, generate_signing_key, public_key_hex, sha256_hex, sign_message_hex,
    signing_key_from_seed_bytes, signing_key_seed_bytes,
};
use froglet_protocol::protocol::{
    ARTIFACT_TYPE_DEAL, ARTIFACT_TYPE_DESCRIPTOR, ARTIFACT_TYPE_OFFER, ARTIFACT_TYPE_QUOTE,
    ARTIFACT_TYPE_RECEIPT, DealPayload, DescriptorCapabilities, DescriptorPayload, ExecutionLimits,
    FROGLET_SCHEMA_V1, OfferExecutionProfile, OfferPayload, OfferPriceSchedule,
    PAYMENT_METHOD_FREE, QuotePayload, QuoteSettlementTerms, ReceiptExecutor, ReceiptLegState,
    ReceiptPayload, ReceiptSettlementLeg, ReceiptSettlementRefs, SETTLEMENT_METHOD_NONE,
    SignedArtifact, sign_artifact,
};
use froglet_verify::{ChainDocumentsReport, validate_chain_documents};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use tokio::sync::{RwLock, broadcast};
use uuid::Uuid;

const CONTAINER_SERVICE_KIND: &str = "compute.container.v1";
const CONTAINER_ABI_VERSION: &str = "biocompass.oci.io.v1";

/// Verify a `froglet/v1` artifact chain offline without network or node state.
pub fn verify_froglet_chain_offline(
    documents: &[Value],
    now: Option<i64>,
) -> ChainDocumentsReport {
    validate_chain_documents(documents, now)
}

/// Persistent `secp256k1` BIP340 Schnorr keypairs for the provider node and local requester.
#[derive(Clone)]
pub struct NodeKeypairSet {
    provider_key: NodeSigningKey,
    requester_key: NodeSigningKey,
}

impl std::fmt::Debug for NodeKeypairSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeKeypairSet")
            .field("provider_id", &self.provider_id())
            .field("requester_id", &self.requester_id())
            .finish()
    }
}

impl NodeKeypairSet {
    /// Load or generate `provider_key.seed` and `requester_key.seed` (`0600`) inside `keys_dir`.
    pub fn load_or_generate(keys_dir: impl AsRef<Path>) -> CompassResult<Self> {
        let dir = keys_dir.as_ref();
        std::fs::create_dir_all(dir)
            .map_err(|e| CompassError::Storage(format!("create keys dir {}: {e}", dir.display())))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
                CompassError::Storage(format!("chmod 0700 keys dir {}: {e}", dir.display()))
            })?;
        }

        let provider_key = load_or_create_seed_file(&dir.join("provider_key.seed"))?;
        let mut requester_key = load_or_create_seed_file(&dir.join("requester_key.seed"))?;
        if public_key_hex(&provider_key) == public_key_hex(&requester_key) {
            requester_key = generate_signing_key();
            write_seed_file(&dir.join("requester_key.seed"), &requester_key)?;
        }

        Ok(Self {
            provider_key,
            requester_key,
        })
    }

    /// Lowercase 64-hex BIP340 x-only public key of the execution node provider.
    pub fn provider_id(&self) -> String {
        public_key_hex(&self.provider_key)
    }

    /// Lowercase 64-hex BIP340 x-only public key of the requester / harness.
    pub fn requester_id(&self) -> String {
        public_key_hex(&self.requester_key)
    }
}

fn load_or_create_seed_file(path: &Path) -> CompassResult<NodeSigningKey> {
    if path.exists() {
        let bytes = std::fs::read(path)
            .map_err(|e| CompassError::Storage(format!("read key seed {}: {e}", path.display())))?;
        let seed: [u8; 32] = bytes.try_into().map_err(|_| {
            CompassError::IntegrityViolation(format!(
                "key seed {} must be exactly 32 bytes",
                path.display()
            ))
        })?;
        return signing_key_from_seed_bytes(&seed).map_err(CompassError::Protocol);
    }

    let key = generate_signing_key();
    write_seed_file(path, &key)?;
    Ok(key)
}

fn write_seed_file(path: &Path, key: &NodeSigningKey) -> CompassResult<()> {
    let seed = signing_key_seed_bytes(key);
    std::fs::write(path, seed)
        .map_err(|e| CompassError::Storage(format!("write key seed {}: {e}", path.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| CompassError::Storage(format!("chmod 0600 {}: {e}", path.display())))?;
    }
    Ok(())
}

/// RAII cleanup guard ensuring ephemeral `/in` & `/out` host stage directories never leak.
struct StageDirGuard {
    path: PathBuf,
}

impl Drop for StageDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Dependency injection bundle for `InProcessNodeRuntime` (zero `if is_cloud` branching).
pub struct NodeRuntimeDeps {
    pub keys: NodeKeypairSet,
    pub session_store: Arc<dyn SessionStore>,
    pub blob_store: Arc<dyn BlobStore>,
    pub oci_registry: Arc<dyn OciRegistryClient>,
    pub payment_gateway: Arc<dyn PaymentGateway>,
    pub oci_runner: Arc<OciContainerRunner>,
    pub stage_root_dir: PathBuf,
    pub allow_local_dev_tags: bool,
}

/// Last-registered version is the unpinned default. Every registered version remains addressable.
#[derive(Default)]
struct PluginReleases {
    default_version: String,
    versions: HashMap<String, LoadedPlugin>,
    registration_order: Vec<String>,
    installed: HashSet<String>,
}

impl PluginReleases {
    fn select(&self, version: Option<&str>) -> Option<&LoadedPlugin> {
        self.versions.get(version.unwrap_or(&self.default_version))
    }
}

/// Bind execution policy and commercial terms separately from editable presentation metadata.
/// This digest occupies the signed Froglet Offer's optional terms_hash and is inherited by its Quote.
pub fn release_terms_hash(plugin: &LoadedPlugin) -> CompassResult<String> {
    let m = &plugin.manifest;
    let mut terms = json!({
        "format": "biocompass.native-module-terms.v1",
        "id": m.id, "version": m.version, "image": m.image, "execution": m.execution,
        "requires_catalog_level": m.requires_catalog_level, "network": m.network,
        "resources": m.resources, "inputs": m.inputs, "offers": m.offers, "price_milli_act": m.price_milli_act
    });
    if let Some(contract) = m.dataset_input_contract.as_deref() {
        terms["dataset_input_contract"] = json!(contract);
    }
    if !m.runtime_queries.is_empty() {
        terms["runtime_queries"] = json!(m.runtime_queries);
    }
    if !m.runtime_join_queries.is_empty() {
        terms["runtime_join_queries"] = json!(m.runtime_join_queries);
    }
    let bytes = canonical_json::to_vec(&terms)
        .map_err(|e| CompassError::Protocol(format!("canonicalize native module terms: {e}")))?;
    Ok(crypto::sha256_hex(&bytes))
}

enum WorkspacePackage {
    Authored {
        manifest: PathBuf,
        detached: PathBuf,
    },
    Managed {
        receipt: PathBuf,
    },
}

struct WorkspacePackageFiles {
    lock: PathBuf,
    package: WorkspacePackage,
}

impl WorkspacePackageFiles {
    fn present(&self) -> bool {
        match &self.package {
            WorkspacePackage::Managed { receipt } => receipt.is_file(),
            WorkspacePackage::Authored { manifest, detached } => {
                manifest.is_file() && !detached.try_exists().unwrap_or(true)
            }
        }
    }

    fn guard(&self, exclusive: bool) -> CompassResult<std::fs::File> {
        let storage = |e: std::io::Error| CompassError::Storage(e.to_string());
        std::fs::create_dir_all(
            self.lock
                .parent()
                .expect("workspace package lock directory"),
        )
        .map_err(storage)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.lock)
            .map_err(storage)?;
        let lock = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        lock.map_err(|e| match e {
            std::fs::TryLockError::WouldBlock => {
                CompassError::IntegrityViolation("this module version is currently in use".into())
            }
            std::fs::TryLockError::Error(error) => storage(error),
        })?;
        Ok(file)
    }
}

/// Local / single-node `RunDispatcher` executing real hardened OCI containers and signing
/// verifiable 5-artifact `froglet/v1` chains.
pub struct InProcessNodeRuntime {
    node_id: NodeId,
    keys: NodeKeypairSet,
    session_store: Arc<dyn SessionStore>,
    blob_store: Arc<dyn BlobStore>,
    oci_registry: Arc<dyn OciRegistryClient>,
    payment_gateway: Arc<dyn PaymentGateway>,
    oci_runner: Arc<OciContainerRunner>,
    plugins: RwLock<HashMap<PluginId, PluginReleases>>,
    catalogs: RwLock<HashMap<CatalogId, DuckDbCatalogEngine>>,
    approved_curated_modules: RwLock<HashSet<(PluginId, String, String)>>,
    stage_root_dir: PathBuf,
    allow_local_dev_tags: bool,
    log_tx: broadcast::Sender<RunLogEvent>,
    terminal_tx: broadcast::Sender<biocompass_core_traits::run_events::TerminalNotice>,
}

impl InProcessNodeRuntime {
    /// Construct a new `InProcessNodeRuntime`.
    pub fn new(deps: NodeRuntimeDeps) -> CompassResult<Self> {
        std::fs::create_dir_all(&deps.stage_root_dir).map_err(|e| {
            CompassError::Storage(format!(
                "create stage_root_dir {}: {e}",
                deps.stage_root_dir.display()
            ))
        })?;
        let runner = deps.oci_runner.for_native_workspace(&deps.stage_root_dir, &deps.keys.provider_id())?;
        let provider_prefix: String = deps.keys.provider_id().chars().take(12).collect();
        let node_id = NodeId::new(format!("froglet-node-{provider_prefix}"))
            .map_err(CompassError::Validation)?;
        let (log_tx, _) = broadcast::channel(1_024);

        Ok(Self {
            node_id,
            keys: deps.keys,
            session_store: deps.session_store,
            blob_store: deps.blob_store,
            oci_registry: deps.oci_registry,
            payment_gateway: deps.payment_gateway,
            oci_runner: Arc::new(runner),
            plugins: RwLock::new(HashMap::new()),
            catalogs: RwLock::new(HashMap::new()),
            approved_curated_modules: RwLock::new(HashSet::new()),
            stage_root_dir: deps.stage_root_dir,
            allow_local_dev_tags: deps.allow_local_dev_tags,
            log_tx,
            terminal_tx: broadcast::channel(256).0,
        })
    }

    pub fn subscribe_terminal(&self) -> broadcast::Receiver<biocompass_core_traits::run_events::TerminalNotice> {
        self.terminal_tx.subscribe()
    }

    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    pub fn provider_id(&self) -> String {
        self.keys.provider_id()
    }

    pub fn requester_id(&self) -> String {
        self.keys.requester_id()
    }

    /// Bind a private local family to this Froglet node. Older workbenches recorded
    /// their requester key as the original publisher, so that key signs its delegation.
    pub fn sign_dataset_owner(&self, original_actor: &str, claim: Value) -> CompassResult<Value> {
        let now = Utc::now().timestamp();
        let kind = "biocompass.dataset-owner.v1";
        let artifact = if original_actor == self.provider_id() {
            self.sign_provider(kind, now, claim)?
        } else if original_actor == self.requester_id() {
            self.sign_requester(kind, now, claim)?
        } else {
            return Err(CompassError::AccessDenied("original local publisher key unavailable".into()));
        };
        serde_json::to_value(artifact).map_err(|e| CompassError::Protocol(e.to_string()))
    }

    /// Sign one domain-separated native dataset action with the existing node key.
    pub fn sign_dataset_action(&self, claim: Value) -> CompassResult<Value> {
        let artifact = self.sign_provider("biocompass.dataset-change.v1", Utc::now().timestamp(), claim)?;
        serde_json::to_value(artifact).map_err(|e| CompassError::Protocol(e.to_string()))
    }

    pub fn verify_dataset_action(signer: &str, claim: &Value, proof: &Value) -> CompassResult<()> {
        Self::verify_dataset_claim("biocompass.dataset-change.v1", signer, claim, proof)
    }

    pub fn verify_dataset_owner(signer: &str, claim: &Value, proof: &Value) -> CompassResult<()> {
        Self::verify_dataset_claim("biocompass.dataset-owner.v1", signer, claim, proof)
    }

    fn verify_dataset_claim(kind: &str, signer: &str, claim: &Value, proof: &Value) -> CompassResult<()> {
        let invalid = || CompassError::IntegrityViolation("invalid Froglet dataset attestation".into());
        let artifact: SignedArtifact<Value> = serde_json::from_value(proof.clone()).map_err(|_| invalid())?;
        if artifact.artifact_type != kind || artifact.signer != signer || &artifact.payload != claim
            || !froglet_protocol::protocol::verify_artifact(&artifact) {
            return Err(invalid());
        }
        Ok(())
    }

    fn bundled_dockerfile_dir(&self, plugin: &LoadedPlugin) -> Option<PathBuf> {
        let root = self.stage_root_dir.parent()?;
        let dir = plugin.root_dir.as_deref()?;
        if dir.parent() == Some(root.join("bundled-plugins").as_path())
            && dir.join("Dockerfile").is_file()
        {
            Some(dir.to_path_buf())
        } else {
            None
        }
    }

    fn bundled_slug<'a>(bundled_dir: &'a Path, plugin: &'a LoadedPlugin) -> &'a str {
        bundled_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_else(|| {
                plugin
                    .manifest
                    .id
                    .as_str()
                    .split_once('/')
                    .map_or(plugin.manifest.id.as_str(), |(_, s)| s)
            })
    }

    /// Return whether a clean-clone local bundled `Dockerfile` fallback image is already built (`R11`).
    pub async fn has_local_bundled_fallback_image(&self, plugin: &LoadedPlugin) -> bool {
        let Some(bundled_dir) = self.bundled_dockerfile_dir(plugin) else {
            return false;
        };
        let slug = Self::bundled_slug(&bundled_dir, plugin);
        self.oci_runner
            .has_local_bundled_image(&bundled_dir, slug, &plugin.manifest.version)
            .await
    }

    /// Build a bundled plugin `Dockerfile` locally when pulling the pinned registry image fails on
    /// a clean clone (`R11`).
    pub async fn ensure_local_bundled_fallback_image(
        &self,
        plugin: &LoadedPlugin,
        pull_err: CompassError,
    ) -> CompassResult<(OciImageRef, Option<String>)> {
        self.resolve_local_bundled_fallback(plugin, pull_err).await
    }

    async fn resolve_local_bundled_fallback(
        &self,
        plugin: &LoadedPlugin,
        registry_err: CompassError,
    ) -> CompassResult<(OciImageRef, Option<String>)> {
        let Some(bundled_dir) = self.bundled_dockerfile_dir(plugin) else {
            return Err(registry_err);
        };
        let slug = Self::bundled_slug(&bundled_dir, plugin);
        let built = self
            .oci_runner
            .build_local_bundled_image(&bundled_dir, slug, &plugin.manifest.version)
            .await?;

        if let Ok(manifest_img) = OciImageRef::parse(&plugin.manifest.image, true)
            && self
                .is_curated_module_approved(
                    &plugin.manifest.id,
                    &plugin.manifest.version,
                    &manifest_img.canonical_module_hash(),
                )
                .await
        {
            self.approve_curated_module(
                plugin.manifest.id.clone(),
                plugin.manifest.version.clone(),
                built.canonical_module_hash(),
            )
            .await;
        }

        let notice = format!(
            "Pinned registry image '{}' was unreachable; built bundled plugins/{slug}/Dockerfile locally ({}) under local dev allowance.",
            plugin.manifest.image, built.raw
        );
        tracing::warn!(
            plugin_id = %plugin.manifest.id,
            pinned_image = %plugin.manifest.image,
            local_image = %built.raw,
            registry_error = %registry_err,
            "{notice}"
        );
        Ok((built, Some(notice)))
    }

    fn workspace_module(&self, p: &LoadedPlugin) -> CompassResult<Option<WorkspacePackageFiles>> {
        let (Some(root), Some(dir)) = (self.stage_root_dir.parent(), p.root_dir.as_deref()) else {
            return Ok(None);
        };
        let scope = serde_json::to_vec(&(&p.manifest.id, &p.manifest.version))
            .map_err(|e| CompassError::Protocol(e.to_string()))?;
        let key = crypto::sha256_hex(&scope);
        let managed = root.join("module-releases").join(&key);
        let package = if dir == managed.join("content") {
            WorkspacePackage::Managed {
                receipt: managed.join("release.json"),
            }
        } else if dir.parent() == Some(root.join("plugins").as_path()) {
            WorkspacePackage::Authored {
                manifest: dir.join("plugin.toml"),
                detached: root.join("detached-modules").join(&key),
            }
        } else {
            return Ok(None);
        };
        Ok(Some(WorkspacePackageFiles {
            lock: root.join("module-locks").join(key),
            package,
        }))
    }

    /// CLI and HTTP processes sharing a data directory take independent OS locks for each release.
    fn module_guard(
        &self,
        p: &LoadedPlugin,
        exclusive: bool,
    ) -> CompassResult<Option<std::fs::File>> {
        self.workspace_module(p)?
            .map(|files| files.guard(exclusive))
            .transpose()
    }

    fn module_present(&self, plugin: &LoadedPlugin) -> bool {
        self.workspace_module(plugin)
            .is_ok_and(|files| files.is_none_or(|f| f.present()))
    }

    fn select_present<'a>(
        &self,
        releases: &'a PluginReleases,
        version: Option<&str>,
    ) -> Option<&'a LoadedPlugin> {
        if let Some(v) = version {
            return releases.select(Some(v)).filter(|p| self.module_present(p));
        }
        releases
            .registration_order
            .iter()
            .rev()
            .filter_map(|v| releases.versions.get(v))
            .find(|p| self.module_present(p))
    }

    /// Register a release. Re-registering its immutable execution terms can update its local UI location.
    pub async fn register_plugin(&self, plugin: LoadedPlugin) -> CompassResult<()> {
        self.register_plugin_with(plugin, false, |_| Ok(())).await
    }

    pub async fn register_workspace_plugin_with(
        &self,
        plugin: LoadedPlugin,
        commit: impl FnOnce(Option<&LoadedPlugin>) -> CompassResult<()>,
    ) -> CompassResult<()> {
        self.register_plugin_with(plugin, false, commit).await
    }

    /// Publish a local package and its registry entry under one release lock.
    /// A failed durable commit leaves the existing in-memory release untouched.
    pub async fn register_installed_plugin(
        &self,
        plugin: LoadedPlugin,
        commit: impl FnOnce(Option<&LoadedPlugin>) -> CompassResult<()>,
    ) -> CompassResult<()> {
        self.register_plugin_with(plugin, true, commit).await
    }

    async fn register_plugin_with(
        &self,
        plugin: LoadedPlugin,
        installed: bool,
        commit: impl FnOnce(Option<&LoadedPlugin>) -> CompassResult<()>,
    ) -> CompassResult<()> {
        plugin.manifest.validate(self.allow_local_dev_tags)?;
        let id = &plugin.manifest.id;
        let version = &plugin.manifest.version;
        let mut plugins = self.plugins.write().await;
        let releases = plugins.entry(id.clone()).or_default();
        let _file_guard = self.module_guard(&plugin, false)?;
        if releases.installed.contains(version) && !installed {
            return Err(CompassError::IntegrityViolation(format!(
                "module '{id}' version '{version}' is an installed marketplace release"
            )));
        }
        if let Some(previous) = releases.versions.get(version)
            && release_terms_hash(previous)? != release_terms_hash(&plugin)? {
                return Err(CompassError::IntegrityViolation(format!(
                    "module '{id}' version '{version}' is already registered with different execution terms"
                )));
            }
        commit(releases.versions.get(version))?;
        if installed {
            releases.installed.insert(version.clone());
        }
        if !releases.versions.contains_key(version) {
            releases.default_version = version.clone();
            releases.registration_order.push(version.clone());
        }
        releases.versions.insert(version.clone(), plugin);
        Ok(())
    }

    /// Approve a specific `(plugin_id, version, canonical_module_hash)` tuple as a curated module
    /// permitted to perform full-scan staging on `compute_only` or row-capped catalogs (§5.3).
    pub async fn approve_curated_module(
        &self,
        plugin_id: PluginId,
        version: impl Into<String>,
        module_hash: impl Into<String>,
    ) {
        self.approved_curated_modules
            .write()
            .await
            .insert((plugin_id, version.into(), module_hash.into()));
    }

    /// Check whether `(plugin_id, version, canonical_module_hash)` is approved for curated full-scan staging.
    pub async fn is_curated_module_approved(
        &self,
        plugin_id: &PluginId,
        version: &str,
        module_hash: &str,
    ) -> bool {
        let guard = self.approved_curated_modules.read().await;
        guard.contains(&(
            plugin_id.clone(),
            version.to_owned(),
            module_hash.to_owned(),
        ))
    }

    /// List all registered plugins sorted deterministically by `PluginId`.
    pub async fn list_plugins(&self) -> Vec<LoadedPlugin> {
        let map = self.plugins.read().await;
        let mut items: Vec<LoadedPlugin> = map.values().filter_map(|releases| self.select_present(releases,None).cloned()).collect();
        items.sort_by(|a, b| a.manifest.id.as_str().cmp(b.manifest.id.as_str()));
        items
    }

    /// Enumerate the exact registered releases in commit order, including authored ones.
    pub async fn plugin_releases(&self, id: &PluginId) -> Vec<LoadedPlugin> {
        let plugins = self.plugins.read().await;
        plugins
            .get(id)
            .map(|r| {
                r.registration_order
                    .iter()
                    .filter_map(|v| {
                        r.versions
                            .get(v)
                            .filter(|p| self.module_present(p))
                            .cloned()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Serialize removal with registration and quote creation. A scientific operation holds its
    /// exact-version usage guard throughout prepare/confirm, independent of other versions.
    pub async fn remove_plugin_with(
        &self,
        id: &PluginId,
        version: &str,
        commit: impl AsyncFnOnce(Option<&LoadedPlugin>) -> CompassResult<bool>,
    ) -> CompassResult<bool> {
        let mut plugins = self.plugins.write().await;
        let Some(releases) = plugins.get_mut(id) else {
            commit(None).await?;
            return Ok(false);
        };
        let existing = releases.versions.get(version);
        let _file_guard = existing.map(|p| self.module_guard(p, true)).transpose()?;
        let removed = commit(existing).await?;
        if existing.is_none() {
            return Ok(false);
        }
        releases.versions.remove(version);
        releases.installed.remove(version);
        releases.registration_order.retain(|v| v != version);
        if let Some(last) = releases.registration_order.last() {
            releases.default_version = last.clone();
        } else {
            plugins.remove(id);
        }
        Ok(removed)
    }

    /// Fetch a registered plugin by ID.
    pub async fn get_plugin(&self, id: &PluginId) -> CompassResult<LoadedPlugin> {
        self.get_plugin_version(id, None).await
    }

    /// Fetch the exact installed release if requested. Only an omitted version selects the default.
    pub async fn get_plugin_version(
        &self,
        id: &PluginId,
        version: Option<&str>,
    ) -> CompassResult<LoadedPlugin> {
        biocompass_core_traits::validate_requested_plugin_version(version)?;
        let plugins = self.plugins.read().await;
        let releases = plugins
            .get(id)
            .ok_or_else(|| CompassError::NotFound(format!("plugin '{id}' is not registered")))?;
        self.select_present(releases, version)
            .cloned()
            .ok_or_else(|| {
                CompassError::NotFound(format!(
                    "plugin '{id}' version '{}' is not installed",
                    version.unwrap_or_default()
                ))
            })
    }

    /// CLI, HTTP and MCP share this native dispatcher. Catalog browsing can list hosted-only modules.
    pub async fn local_execution_plugin(
        &self,
        id: &PluginId,
        version: Option<&str>,
    ) -> CompassResult<(LoadedPlugin, Option<std::fs::File>)> {
        biocompass_core_traits::validate_requested_plugin_version(version)?;
        let plugins = self.plugins.read().await;
        let releases = plugins
            .get(id)
            .ok_or_else(|| CompassError::NotFound(format!("plugin '{id}' is not registered")))?;
        let plugin = self.select_present(releases, version).ok_or_else(|| {
            CompassError::NotFound(format!(
                "plugin '{id}' version '{}' is not installed",
                version.unwrap_or_default()
            ))
        })?;
        if plugin.manifest.execution == PluginExecutionPlane::CloudOnly {
            return Err(CompassError::AccessDenied(format!(
                "plugin '{id}' version '{}' declares execution='cloud_only' and cannot run locally",
                plugin.manifest.version
            )));
        }
        let file = self.module_guard(plugin, false)?;
        if !self.module_present(plugin) {
            return Err(CompassError::NotFound(format!("module '{id}' was removed")));
        }
        if let Some(files) = self.workspace_module(plugin)? {
            let manifest = match files.package {
                WorkspacePackage::Managed { receipt } => {
                    #[derive(serde::Deserialize)]
                    struct Metadata {
                        manifest: biocompass_plugin_spec::PluginManifest,
                    }
                    let input = std::fs::File::open(receipt)
                        .map_err(|e| CompassError::Storage(e.to_string()))?;
                    serde_json::from_reader::<_, Metadata>(input)
                        .map_err(|e| CompassError::Storage(e.to_string()))?
                        .manifest
                }
                WorkspacePackage::Authored { manifest, .. } => {
                    let text = std::fs::read_to_string(manifest)
                        .map_err(|e| CompassError::Storage(e.to_string()))?;
                    biocompass_plugin_spec::PluginManifest::from_toml_str(
                        &text,
                        self.allow_local_dev_tags,
                    )?
                }
            };
            let current = LoadedPlugin {
                manifest,
                skill: None,
                root_dir: None,
            };
            if release_terms_hash(&current)? != release_terms_hash(plugin)? {
                return Err(CompassError::IntegrityViolation(
                    "workspace module terms changed; reload the workspace".into(),
                ));
            }
        }
        Ok((plugin.clone(), file))
    }

    /// Register an integrity-verified `DuckDbCatalogEngine`.
    pub async fn register_catalog(&self, catalog: DuckDbCatalogEngine) -> CompassResult<()> {
        catalog.verify_integrity()?;
        self.catalogs
            .write()
            .await
            .insert(catalog.manifest().id.clone(), catalog);
        Ok(())
    }

    /// Keep a registered physical catalog bound to one immutable seal.
    pub async fn register_immutable_catalog(&self, catalog: DuckDbCatalogEngine) -> CompassResult<()> {
        catalog.verify_integrity()?;
        let id = catalog.summary().id.clone();
        let mut catalogs = self.catalogs.write().await;
        if let Some(existing) = catalogs.get(&id) {
            if existing.summary().latest_sealed != catalog.summary().latest_sealed {
                return Err(CompassError::IntegrityViolation(format!(
                    "physical catalog '{id}' is already bound to a different immutable seal"
                )));
            }
        } else {
            catalogs.insert(id, catalog);
        }
        Ok(())
    }

    /// List all registered catalogs sorted deterministically by `CatalogId`.
    pub async fn list_catalogs(&self) -> Vec<DuckDbCatalogEngine> {
        let map = self.catalogs.read().await;
        let mut items: Vec<DuckDbCatalogEngine> = map.values().cloned().collect();
        items.sort_by(|a, b| a.manifest().id.as_str().cmp(b.manifest().id.as_str()));
        items
    }

    /// Fetch a registered catalog engine by ID.
    pub async fn get_catalog(&self, id: &CatalogId) -> CompassResult<DuckDbCatalogEngine> {
        self.catalogs
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| CompassError::NotFound(format!("catalog '{id}' is not registered")))
    }

    /// Mutate a registered catalog engine in place (e.g., pin/unpin/expire/rollback/commit_mutation).
    pub async fn mutate_catalog<F, R>(&self, id: &CatalogId, f: F) -> CompassResult<R>
    where
        F: FnOnce(&mut DuckDbCatalogEngine) -> CompassResult<R>,
    {
        let mut map = self.catalogs.write().await;
        let eng = map
            .get_mut(id)
            .ok_or_else(|| CompassError::NotFound(format!("catalog '{id}' is not registered")))?;
        f(eng)
    }

    async fn stage_private_hla_inputs(
        &self,
        plugin_id: PluginId,
        grant: &DatasetGrant,
        params: &Value,
        input_dir: &Path,
        max_input_bytes: usize,
    ) -> CompassResult<()> {
        let catalog = self.get_catalog(&grant.catalog_id).await?;
        let params = params.clone();
        let scope = grant.row_scope.clone();
        let data_dir = input_dir.join("data");
        let input_dir = input_dir.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let (version, sealed) = private_hla_pin(&catalog, &plugin_id, &params)?;
            let storage_error = |e: std::io::Error| CompassError::Storage(e.to_string());
            std::fs::create_dir(&data_dir).map_err(storage_error)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o755))
                    .map_err(storage_error)?;
            }
            materialize_reviewed_hla_inputs(
                &catalog,
                &plugin_id,
                version,
                sealed.snapshot_id,
                &sealed.dataset_digest,
                &params,
                scope.as_ref(),
                &data_dir,
            )?;
            for entry in std::fs::read_dir(&data_dir).map_err(storage_error)? {
                let path = entry.map_err(storage_error)?.path();
                set_world_readable_file(&path)?;
            }
            let bytes = measure_dir_regular_bytes(&input_dir).map_err(CompassError::Storage)?;
            if bytes > max_input_bytes as u64 {
                return Err(CompassError::Validation(format!(
                    "INPUT_LIMIT_EXCEEDED: staged inputs are {bytes} bytes, exceeding signed max_input_bytes ({max_input_bytes})"
                )));
            }
            Ok(())
        })
        .await
        .map_err(|e| CompassError::Internal(format!("Private HLA input staging task: {e}")))?
    }

    fn emit_stage_log(&self, run_id: &RunId, line: impl Into<String>) {
        let _ = self.log_tx.send(RunLogEvent {
            run_id: run_id.clone(),
            timestamp: Utc::now(),
            stream: "stage".into(),
            line: line.into(),
        });
    }

    fn sign_provider<T: serde::Serialize + Clone>(
        &self,
        artifact_type: &str,
        created_at: i64,
        payload: T,
    ) -> CompassResult<SignedArtifact<T>> {
        let provider_id = self.keys.provider_id();
        sign_artifact(
            &provider_id,
            |msg| sign_message_hex(&self.keys.provider_key, msg),
            artifact_type,
            created_at,
            payload,
        )
        .map_err(CompassError::Protocol)
    }

    fn sign_requester<T: serde::Serialize + Clone>(
        &self,
        artifact_type: &str,
        created_at: i64,
        payload: T,
    ) -> CompassResult<SignedArtifact<T>> {
        let requester_id = self.keys.requester_id();
        sign_artifact(
            &requester_id,
            |msg| sign_message_hex(&self.keys.requester_key, msg),
            artifact_type,
            created_at,
            payload,
        )
        .map_err(CompassError::Protocol)
    }

    /// Execute everything inside a reserved ACT + signed `Deal` scope so every outcome produces
    /// a cryptographically verified 5-artifact chain (`succeeded` or `failed`) and never leaks
    /// reserved ACT credits.
    #[allow(clippy::too_many_arguments)]
    async fn execute_confirmed_deal(
        &self,
        mut run: RunRecord,
        plugin: &LoadedPlugin,
        verified_image: &OciImageRef,
        descriptor_doc: Value,
        offer_doc: Value,
        quote: &SignedArtifact<QuotePayload>,
        quote_doc: Value,
        deal: &SignedArtifact<DealPayload>,
        deal_doc: Value,
        pinned_snapshot: Option<(u64, String)>,
        revoked: Option<&AtomicBool>,
        queries: Option<Arc<dyn biocompass_oci_runner::LiveQueryResponder>>,
    ) -> CompassResult<RunRecord> {
        let run_id = run.run_id.clone();
        let stage_dir = self
            .stage_root_dir
            .join(format!("stage-{}-{}", run_id.as_str(), Uuid::new_v4().simple()));
        let _stage_guard = StageDirGuard {
            path: stage_dir.clone(),
        };
        let in_dir = stage_dir.join("in");
        let out_dir = stage_dir.join("out");

        let started_at = Utc::now().timestamp();
        let outcome = self
            .stage_and_run_container(
                &mut run,
                plugin,
                verified_image,
                &quote.payload.execution_limits,
                &stage_dir,
                &in_dir,
                &out_dir,
                pinned_snapshot.as_ref(),
                revoked,
                queries,
            )
            .await;
        let outcome = if revoked.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            match outcome {
                Err((_, message, engine)) if message.contains("OCI_CLEANUP_UNCONFIRMED") =>
                    Err(("DATASET_ACCESS_REVOKED".into(), format!("Dataset access revoked. {message}"), engine)),
                _ => Err(("DATASET_ACCESS_REVOKED".into(), "Dataset access revoked during execution".into(), self.oci_runner.engine().server_version.clone())),
            }
        } else { outcome };
        let finished_at = Utc::now().timestamp().max(started_at);

        let capabilities = quote.payload.capabilities_granted.clone();
        let execution_mode = if plugin.manifest.network.is_airgapped() {
            "airgapped".to_string()
        } else {
            "egress_allowlist".to_string()
        };

        let (status, result_json, artifacts, receipt_payload, err_opt) =
            match outcome {
                Ok(success) => {
                    let payload = ReceiptPayload {
                        provider_id: self.keys.provider_id(),
                        requester_id: self.keys.requester_id(),
                        deal_hash: deal.hash.clone(),
                        quote_hash: quote.hash.clone(),
                        extension_refs: Vec::new(),
                        acceptance_ref: None,
                        started_at: Some(started_at),
                        finished_at,
                        deal_state: "succeeded".into(),
                        execution_state: "succeeded".into(),
                        settlement_state: SETTLEMENT_METHOD_NONE.into(),
                        result_hash: Some(success.result_hash),
                        confidential_session_hash: None,
                        result_envelope_hash: None,
                        result_format: Some("application/json".into()),
                        executor: ReceiptExecutor {
                            runtime: ExecutionRuntime::Container.as_str().to_string(),
                            runtime_version: success.engine_version.clone(),
                            execution_mode: Some(execution_mode),
                            attestation_platform: None,
                            measurement: None,
                            abi_version: Some(CONTAINER_ABI_VERSION.into()),
                            module_hash: Some(verified_image.canonical_module_hash()),
                            capabilities_granted: capabilities,
                        },
                        limits_applied: quote.payload.execution_limits.clone(),
                        settlement_refs: empty_free_settlement_refs(),
                        failure_code: None,
                        failure_message: None,
                        result_ref: Some(format!("blob://runs/{}/result.json", run_id.as_str())),
                    };
                    (
                        RunStatus::Succeeded,
                        Some(success.result_value),
                        success.artifacts,
                        payload,
                        None,
                    )
                }
                Err((code, message, engine_ver)) => {
                    let payload = ReceiptPayload {
                        provider_id: self.keys.provider_id(),
                        requester_id: self.keys.requester_id(),
                        deal_hash: deal.hash.clone(),
                        quote_hash: quote.hash.clone(),
                        extension_refs: Vec::new(),
                        acceptance_ref: None,
                        started_at: Some(started_at),
                        finished_at,
                        deal_state: "failed".into(),
                        execution_state: "failed".into(),
                        settlement_state: SETTLEMENT_METHOD_NONE.into(),
                        result_hash: None,
                        confidential_session_hash: None,
                        result_envelope_hash: None,
                        result_format: None,
                        executor: ReceiptExecutor {
                            runtime: ExecutionRuntime::Container.as_str().to_string(),
                            runtime_version: engine_ver.clone(),
                            execution_mode: Some(execution_mode),
                            attestation_platform: None,
                            measurement: None,
                            abi_version: Some(CONTAINER_ABI_VERSION.into()),
                            module_hash: Some(verified_image.canonical_module_hash()),
                            capabilities_granted: capabilities,
                        },
                        limits_applied: quote.payload.execution_limits.clone(),
                        settlement_refs: empty_free_settlement_refs(),
                        failure_code: Some(code),
                        failure_message: Some(message.clone()),
                        result_ref: None,
                    };
                    (
                        RunStatus::Failed,
                        None,
                        Vec::new(),
                        payload,
                        Some(message),
                    )
                }
            };

        let receipt = self.sign_provider(ARTIFACT_TYPE_RECEIPT, finished_at, receipt_payload)?;
        let receipt_doc = serde_json::to_value(&receipt)
            .map_err(|e| CompassError::Internal(format!("serialize receipt: {e}")))?;
        let chain_docs = vec![
            descriptor_doc,
            offer_doc,
            quote_doc,
            deal_doc,
            receipt_doc.clone(),
        ];

        let chain_report = validate_chain_documents(&chain_docs, None);
        if !chain_report.valid {
            let reason = format!(
                "froglet-verify self-check rejected generated chain: {}",
                serde_json::to_string(&chain_report).unwrap_or_default()
            );
            return Err(CompassError::Protocol(reason));
        }

        let chain_bytes = serde_json::to_vec_pretty(&chain_docs)
            .map_err(|e| CompassError::Internal(format!("encode froglet_chain.json: {e}")))?;
        self.blob_store
            .put_bytes(
                &format!("runs/{}/froglet_chain.json", run_id.as_str()),
                &chain_bytes,
            )
            .await?;

        run.status = status;
        run.result_json = result_json;
        run.artifacts = artifacts;
        run.froglet_receipt = Some(receipt_doc);
        run.froglet_chain = chain_docs;
        run.receipt_verified = true;
        run.error_message = err_opt;
        run.completed_at = Some(Utc::now());

        self.post_and_finish(run).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn stage_and_run_container(
        &self,
        run: &mut RunRecord,
        plugin: &LoadedPlugin,
        verified_image: &OciImageRef,
        limits: &ExecutionLimits,
        stage_dir: &Path,
        in_dir: &Path,
        out_dir: &Path,
        pinned_snapshot: Option<&(u64, String)>,
        revoked: Option<&AtomicBool>,
        queries: Option<Arc<dyn biocompass_oci_runner::LiveQueryResponder>>,
    ) -> Result<StageSuccessOutcome, (String, String, String)> {
        let default_engine_ver = format!(
            "{}:{}",
            self.oci_runner.engine().kind.as_str(),
            self.oci_runner.engine().server_version
        );

        prepare_stage_permissions(stage_dir, in_dir, out_dir)
            .map_err(|e| ("STAGE_PREPARE_ERROR".into(), e.to_string(), default_engine_ver.clone()))?;

        // 1. Write canonical /in/params.json and enforce signed max_input_bytes (§6.2).
        let params_bytes = canonical_json::to_vec(&run.params).map_err(|e| {
            (
                "PARAMS_ENCODE_ERROR".into(),
                format!("canonicalize params.json: {e}"),
                default_engine_ver.clone(),
            )
        })?;
        if params_bytes.len() > limits.max_input_bytes {
            return Err((
                "INPUT_LIMIT_EXCEEDED".into(),
                format!(
                    "canonical params.json is {} bytes, exceeding signed max_input_bytes ({})",
                    params_bytes.len(),
                    limits.max_input_bytes
                ),
                default_engine_ver,
            ));
        }
        let params_path = in_dir.join("params.json");
        std::fs::write(&params_path, &params_bytes).map_err(|e| {
            (
                "STAGE_PARAMS_WRITE_ERROR".into(),
                format!("write /in/params.json: {e}"),
                default_engine_ver.clone(),
            )
        })?;
        set_world_readable_file(&params_path).map_err(|e| {
            (
                "STAGE_PERMS_ERROR".into(),
                e.to_string(),
                default_engine_ver.clone(),
            )
        })?;

        // 2. Supply private named projections or the legacy single-table Parquet slice.
        if queries.is_some() {
            // The reviewed live protocol delivers only the declared read responses.
        } else if let Some(grant) = run.grant.as_ref().filter(|grant| is_private_catalog(grant)) {
            self.stage_private_hla_inputs(
                plugin.manifest.id.clone(), grant, &run.params, in_dir, limits.max_input_bytes,
            )
                .await
                .map_err(|e| (
                    "PRIVATE_DATASET_STAGE_ERROR".into(), e.to_string(), default_engine_ver.clone()
                ))?;
        } else if let Some(grant) = &run.grant {
            let catalog = self.get_catalog(&grant.catalog_id).await.map_err(|e| {
                (
                    "CATALOG_LOOKUP_ERROR".into(),
                    e.to_string(),
                    default_engine_ver.clone(),
                )
            })?;
            let mut filter_query: QueryAst = run
                .params
                .get("cohort_filter")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| {
                    (
                        "COHORT_FILTER_INVALID".into(),
                        format!("invalid cohort_filter: {e}"),
                        default_engine_ver.clone(),
                    )
                })?
                .unwrap_or_default();
            // SEC-31: Pin the exact snapshot_id verified at confirm time to eliminate provenance TOCTOU.
            if let Some((pinned_sid, _)) = pinned_snapshot {
                filter_query.snapshot_id = Some(*pinned_sid);
            }

            let is_curated = self
                .is_curated_module_approved(
                    &plugin.manifest.id,
                    &plugin.manifest.version,
                    &verified_image.canonical_module_hash(),
                )
                .await;
            let staging_mode = if is_curated {
                StagingMode::CuratedFullScan
            } else {
                StagingMode::CustomerScoped
            };

            let data_dir = in_dir.join("data");
            std::fs::create_dir_all(&data_dir).map_err(|e| {
                (
                    "STAGE_DATA_DIR_ERROR".into(),
                    format!("create /in/data: {e}"),
                    default_engine_ver.clone(),
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o755));
            }
            let cohort_parquet = data_dir.join("cohort.parquet");
            let slice = catalog
                .materialize_parquet_for_run(grant, &filter_query, staging_mode, &cohort_parquet)
                .map_err(|e| {
                    (
                        "DUCKDB_STAGE_ERROR".into(),
                        e.to_string(),
                        default_engine_ver.clone(),
                    )
                })?;
            if let Some((pinned_sid, expected_digest)) = pinned_snapshot
                && (slice.snapshot_id != *pinned_sid || slice.dataset_digest != *expected_digest)
            {
                return Err((
                    "SNAPSHOT_PROVENANCE_MISMATCH".into(),
                    format!(
                        "staged snapshot ({}, {}) diverged from confirmed snapshot ({}, {})",
                        slice.snapshot_id, slice.dataset_digest, pinned_sid, expected_digest
                    ),
                    default_engine_ver.clone(),
                ));
            }
            set_world_readable_file(&cohort_parquet).map_err(|e| {
                (
                    "STAGE_PERMS_ERROR".into(),
                    e.to_string(),
                    default_engine_ver.clone(),
                )
            })?;
            let _ = self
                .mutate_catalog(&grant.catalog_id, |c| c.pin_snapshot(slice.snapshot_id))
                .await;
            let stage_msg = format!(
                "Staged DuckLake cohort slice ({} rows, level={}, snapshot={}, sha256={})",
                slice.row_count,
                grant.level.as_str(),
                slice.snapshot_id,
                &slice.sha256[..12]
            );
            run.logs.push(format!("[stage] {stage_msg}"));
            self.emit_stage_log(&run.run_id, stage_msg);
        }

        // 3. Execute hardened OCI container enforcing signed Offer runtime & memory limits (§6.2).
        let mut enforced_resources = plugin.manifest.resources.clone();
        enforced_resources.timeout_sec = (limits.max_runtime_ms / 1_000).max(1);
        enforced_resources.memory_mb = ((limits.max_memory_bytes / (1024 * 1024)) as u64).max(64);

        self.emit_stage_log(
            &run.run_id,
            format!(
                "Launching hardened OCI container {} (timeout={}s, memory={}MB)",
                verified_image.raw, enforced_resources.timeout_sec, enforced_resources.memory_mb
            ),
        );
        let spec = ContainerRunSpec {
            run_id: run.run_id.clone(),
            image: verified_image.clone(),
            in_dir: in_dir.to_path_buf(),
            out_dir: out_dir.to_path_buf(),
            resources: enforced_resources,
            network: plugin.manifest.network.clone(),
            command_override: None,
        };

        let report = self
            .oci_runner
            .execute_with_limits(
                &spec,
                Some(self.log_tx.clone()),
                revoked,
                queries,
                None,
                Some(limits.max_output_bytes as u64),
            )
            .await
            .map_err(|e| {
                let msg = e.to_string();
                run.logs.push(format!("[error] {msg}"));
                let code = if msg.contains("DATASET_ACCESS_REVOKED:") {
                    "DATASET_ACCESS_REVOKED"
                } else if msg.contains("OUTPUT_QUOTA_EXCEEDED:") {
                    "OUTPUT_QUOTA_EXCEEDED"
                } else if msg.contains("CONTAINER_TIMEOUT:") {
                    "CONTAINER_TIMEOUT"
                } else {
                    "CONTAINER_EXECUTION_ERROR"
                };
                (code.into(), msg, default_engine_ver.clone())
            })?;

        let engine_version = format!("{}:{}", report.engine.as_str(), report.engine_version);
        for line in &report.stdout_lines {
            run.logs.push(format!("[stdout] {line}"));
        }
        for line in &report.stderr_lines {
            run.logs.push(format!("[stderr] {line}"));
        }

        // Enforce signed max_output_bytes across ALL /out files + stdout/stderr BEFORE reading into RAM (§6.2).
        let out_dir_bytes = measure_dir_regular_bytes(out_dir).map_err(|msg| {
            (
                "INVALID_OUTPUT_TREE".into(),
                msg,
                engine_version.clone(),
            )
        })?;
        let total_output_bytes = out_dir_bytes.saturating_add(report.log_bytes_total);
        if total_output_bytes > (limits.max_output_bytes as u64) {
            return Err((
                "OUTPUT_LIMIT_EXCEEDED".into(),
                format!(
                    "combined /out ({out_dir_bytes} B) + stdout/stderr ({} B) = {total_output_bytes} B exceeds signed max_output_bytes ({} B)",
                    report.log_bytes_total, limits.max_output_bytes
                ),
                engine_version,
            ));
        }

        if report.exit_code != 0 {
            let tail = report
                .stderr_lines
                .last()
                .or_else(|| report.stdout_lines.last())
                .cloned()
                .unwrap_or_else(|| "no stderr output".into());
            let code = if report.exit_code == 137 {
                "MEMORY_OR_KILL_EXIT_137".to_string()
            } else {
                format!("EXIT_CODE_{}", report.exit_code)
            };
            return Err((
                code,
                format!(
                    "Container exited with non-zero code {}: {tail}",
                    report.exit_code
                ),
                engine_version,
            ));
        }

        // 4. Stage-Out /out/result.json and /out/artifacts/* into BlobStore.
        let result_path = out_dir.join("result.json");
        let result_meta = std::fs::symlink_metadata(&result_path).map_err(|e| {
            (
                "MISSING_RESULT_JSON".into(),
                format!("container exited 0 without writing /out/result.json: {e}"),
                engine_version.clone(),
            )
        })?;
        if result_meta.file_type().is_symlink() || !result_meta.is_file() {
            return Err((
                "INVALID_RESULT_FILE".into(),
                "/out/result.json must be a regular file, not a symlink".into(),
                engine_version,
            ));
        }
        let result_bytes = std::fs::read(&result_path).map_err(|e| {
            (
                "READ_RESULT_ERROR".into(),
                format!("read /out/result.json: {e}"),
                engine_version.clone(),
            )
        })?;
        let result_value: Value = serde_json::from_slice(&result_bytes).map_err(|e| {
            (
                "INVALID_RESULT_JSON".into(),
                format!("/out/result.json is not valid JSON: {e}"),
                engine_version.clone(),
            )
        })?;

        let result_blob_key = format!("runs/{}/result.json", run.run_id.as_str());
        let result_sha256 = self
            .blob_store
            .put_bytes(&result_blob_key, &result_bytes)
            .await
            .map_err(|e| {
                (
                    "BLOB_STORE_ERROR".into(),
                    e.to_string(),
                    engine_version.clone(),
                )
            })?;

        let artifacts = self
            .collect_output_artifacts(&run.run_id, &out_dir.join("artifacts"))
            .await
            .map_err(|e| ("ARTIFACT_STAGE_OUT_ERROR".into(), e.to_string(), engine_version.clone()))?;

        let result_hash = biocompass_core_traits::result_manifest_hash(&result_sha256, &artifacts)
            .map_err(|error| ("RESULT_HASH_ERROR".into(), error.to_string(), engine_version.clone()))?;

        Ok(StageSuccessOutcome {
            result_value,
            artifacts,
            result_hash,
            engine_version,
        })
    }

    async fn collect_output_artifacts(
        &self,
        run_id: &RunId,
        artifacts_dir: &Path,
    ) -> CompassResult<Vec<ArtifactRef>> {
        if !artifacts_dir.exists() {
            return Ok(Vec::new());
        }
        let mut files = Vec::new();
        collect_regular_files_recursive(artifacts_dir, artifacts_dir, &mut files)?;
        files.sort_by(|a, b| a.0.cmp(&b.0));

        let mut refs = Vec::with_capacity(files.len());
        for (rel_name, full_path, size_bytes) in files {
            let bytes = std::fs::read(&full_path).map_err(|e| {
                CompassError::Storage(format!("read output artifact {}: {e}", full_path.display()))
            })?;
            let blob_key = format!("runs/{}/artifacts/{rel_name}", run_id.as_str());
            let sha256 = self.blob_store.put_bytes(&blob_key, &bytes).await?;
            let media_type = mime_guess::from_path(&rel_name)
                .first_or_octet_stream()
                .essence_str()
                .to_owned();
            refs.push(ArtifactRef {
                name: rel_name,
                blob_key,
                media_type,
                size_bytes,
                sha256,
            });
        }
        Ok(refs)
    }
}

struct StageSuccessOutcome {
    result_value: Value,
    artifacts: Vec<ArtifactRef>,
    result_hash: String,
    engine_version: String,
}

pub fn measure_dir_regular_bytes(dir: &Path) -> Result<u64, String> {
    if !dir.exists() {
        return Ok(0);
    }
    let mut total: u64 = 0;
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read entry in {}: {e}", dir.display()))?;
        let path = entry.path();
        let meta =
            std::fs::symlink_metadata(&path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!("refusing symlink inside /out: {}", path.display()));
        }
        if meta.is_dir() {
            total = total.saturating_add(measure_dir_regular_bytes(&path)?);
        } else if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    Ok(total)
}

fn collect_regular_files_recursive(
    root: &Path,
    current: &Path,
    out: &mut Vec<(String, PathBuf, u64)>,
) -> CompassResult<()> {
    let entries = std::fs::read_dir(current).map_err(|e| {
        CompassError::Storage(format!("read artifacts dir {}: {e}", current.display()))
    })?;
    for entry in entries {
        let entry =
            entry.map_err(|e| CompassError::Storage(format!("read artifact entry: {e}")))?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).map_err(|e| {
            CompassError::Storage(format!("stat output artifact {}: {e}", path.display()))
        })?;
        if meta.file_type().is_symlink() {
            return Err(CompassError::IntegrityViolation(format!(
                "refusing symlink inside /out/artifacts: {}",
                path.display()
            )));
        }
        if meta.is_dir() {
            collect_regular_files_recursive(root, &path, out)?;
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| CompassError::Internal(format!("strip artifact prefix: {e}")))?
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path, meta.len()));
        }
    }
    Ok(())
}

fn set_world_readable_file(path: &Path) -> CompassResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))
            .map_err(|e| CompassError::Storage(format!("chmod 0644 {}: {e}", path.display())))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn empty_free_settlement_refs() -> ReceiptSettlementRefs {
    let empty_leg = ReceiptSettlementLeg {
        amount_msat: 0,
        invoice_hash: String::new(),
        payment_hash: String::new(),
        state: ReceiptLegState::Canceled,
    };
    ReceiptSettlementRefs {
        method: SETTLEMENT_METHOD_NONE.into(),
        bundle_hash: None,
        destination_identity: String::new(),
        base_fee: empty_leg.clone(),
        success_fee: empty_leg,
    }
}

fn is_private_catalog(grant: &DatasetGrant) -> bool {
    grant.catalog_id.as_str().split_once('/').is_some_and(|(namespace, _)| {
        namespace.eq_ignore_ascii_case("bc-private-v1")
    })
}

fn live_query_grant(plugin: &LoadedPlugin, grant: &DatasetGrant) -> bool {
    if plugin.manifest.runtime_join_queries.is_empty() {
        is_private_catalog(grant)
    } else {
        !is_private_catalog(grant)
            && grant.level == GrantLevel::ComputeOnly
            && grant.row_scope.is_some()
            && grant.daily_row_budget.is_none()
            && grant.max_rows_per_query == 0
            && !grant.allow_local_export
    }
}

pub fn require_reviewed_hla_module(
    plugin: &LoadedPlugin,
    image: &OciImageRef,
    grant: &DatasetGrant,
) -> CompassResult<()> {
    let contract = reviewed_hla_input_contract(&plugin.manifest.id);
    if contract.is_none()
        || plugin.manifest.dataset_input_contract.as_deref() != contract
        || !plugin.manifest.network.is_airgapped()
        || image.sha256_digest.is_none()
        || grant.level != GrantLevel::ComputeOnly
        || grant.daily_row_budget.is_some()
        || grant.max_rows_per_query != 0
        || grant.allow_local_export
    {
        return Err(CompassError::AccessDenied(
            "private datasets require a reviewed, digest-pinned HLA module and a compute-only grant".into(),
        ));
    }
    target_validation::SampleScope::parse(grant.row_scope.as_ref())?;
    Ok(())
}

fn private_hla_pin(
    catalog: &DuckDbCatalogEngine,
    plugin_id: &PluginId,
    params: &Value,
) -> CompassResult<(u64, SealedSnapshot)> {
    let version = params.get("dataset_version").and_then(Value::as_u64).filter(|n| *n > 0)
        .ok_or_else(|| CompassError::Validation("private dataset_version must be pinned".into()))?;
    let snapshot_id = params.pointer("/cohort_filter/snapshot_id").and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .ok_or_else(|| CompassError::Validation("private cohort_filter.snapshot_id must be pinned".into()))?;
    if params.get("cohort_filter").and_then(Value::as_object).is_none_or(|fields| fields.len() != 1) {
        return Err(CompassError::Validation("private cohort_filter accepts only snapshot_id".into()));
    }
    if catalog.summary().version != version.to_string() || catalog.summary().latest_snapshot_id != snapshot_id {
        return Err(CompassError::IntegrityViolation(
            "private dataset version and snapshot do not identify the registered release".into(),
        ));
    }
    let sealed = catalog.load_sealed_snapshot(Some(snapshot_id))?;
    validate_reviewed_hla_inputs(plugin_id, &sealed, params)?;
    Ok((version, sealed))
}

fn hla_contract(plugin_id: &PluginId) -> CompassResult<&'static str> {
    reviewed_hla_input_contract(plugin_id)
        .ok_or_else(|| CompassError::AccessDenied("unsupported private HLA module".into()))
}

/// Validate physical sealed columns before creating a private run quote.
pub fn validate_reviewed_hla_inputs(
    plugin_id: &PluginId,
    snapshot: &SealedSnapshot,
    params: &Value,
) -> CompassResult<()> {
    target_validation::validate_scientific_schema(hla_contract(plugin_id)?, snapshot, params)
}

pub use biocompass_plugin_spec::validate_reviewed_hla_capabilities;

/// Supply scoped, named inputs using sealed shards, also usable with the worker's cached engine.
#[allow(clippy::too_many_arguments)]
pub fn materialize_reviewed_hla_inputs(
    engine: &DuckDbCatalogEngine,
    plugin_id: &PluginId,
    dataset_version: u64,
    snapshot_id: u64,
    digest: &str,
    params: &Value,
    row_scope: Option<&FilterExpr>,
    out_dir: &Path,
) -> CompassResult<()> {
    target_validation::materialize_scientific(
        engine, engine.summary().id.as_str(), dataset_version, snapshot_id, digest,
        hla_contract(plugin_id)?, params, out_dir, row_scope,
    )
}

fn compute_canonical_workload_hash(
    plugin: &LoadedPlugin,
    verified_image: &OciImageRef,
    params: &Value,
    dataset_digest: Option<&str>,
    grant: Option<&DatasetGrant>,
) -> CompassResult<String> {
    let workload_descriptor = json!({
        "plugin_id": plugin.manifest.id,
        "plugin_version": plugin.manifest.version,
        "image_digest": verified_image.canonical_module_hash(),
        "params": params,
        "dataset_digest": dataset_digest,
        "grant_id": grant.map(|g| &g.grant_id),
        "access_level": grant.map(|g| g.level.as_str()),
        "row_scope": grant.and_then(|g| g.row_scope.as_ref()),
    });
    let workload_bytes = canonical_json::to_vec(&workload_descriptor)
        .map_err(|e| CompassError::Protocol(format!("canonicalize workload: {e}")))?;
    Ok(crypto::sha256_hex(&workload_bytes))
}

#[async_trait]
impl RunDispatcher for InProcessNodeRuntime {
    async fn prepare_run(&self, req: PrepareRunRequest) -> CompassResult<RunQuote> {
        let (plugin, _active_release) = self.local_execution_plugin(&req.plugin_id, req.plugin_version.as_deref()).await?;
        plugin.manifest.validate_dataset_binding(req.dataset_grant.is_some())?;
        if plugin.manifest.has_live_queries()
            && !req.dataset_grant.as_ref().is_some_and(|grant| live_query_grant(&plugin, grant))
        {
            return Err(CompassError::AccessDenied(
                "live reads require a trusted private dataset provider".into(),
            ));
        }

        let (verified_image, execution_notice) = match self
            .oci_registry
            .verify_image(&plugin.manifest.image)
            .await
        {
            Ok(img) => (img, None),
            Err(err) => self.resolve_local_bundled_fallback(&plugin, err).await?,
        };

        let mut dataset_digest: Option<String> = None;
        let mut grant_summary: Option<String> = None;
        if let Some(grant) = &req.dataset_grant {
            if grant.org_id != req.org_id {
                return Err(CompassError::AccessDenied(format!(
                    "dataset grant '{}' belongs to org '{}', not '{}'",
                    grant.grant_id, grant.org_id, req.org_id
                )));
            }
            if grant.level < plugin.manifest.requires_catalog_level {
                return Err(CompassError::AccessDenied(format!(
                    "plugin '{}' requires '{}' catalog access, but grant '{}' is '{}'",
                    plugin.manifest.id,
                    plugin.manifest.requires_catalog_level.as_str(),
                    grant.grant_id,
                    grant.level.as_str()
                )));
            }
            if grant.level == GrantLevel::ComputeOnly
                && !self
                    .is_curated_module_approved(
                        &plugin.manifest.id,
                        &plugin.manifest.version,
                        &verified_image.canonical_module_hash(),
                    )
                    .await
            {
                return Err(CompassError::AccessDenied(format!(
                    "compute_only catalog '{}' only permits approved curated modules; '{}' is not approved for full-scan execution",
                    grant.catalog_id, plugin.manifest.id
                )));
            }
            let catalog = self.get_catalog(&grant.catalog_id).await?;
            catalog.verify_integrity()?;
            let filter_query: QueryAst = req
                .params
                .get("cohort_filter")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| CompassError::Validation(format!("invalid cohort_filter: {e}")))?
                .unwrap_or_default();
            let requested_sid = filter_query.snapshot_id;
            let sealed = if is_private_catalog(grant) {
                require_reviewed_hla_module(&plugin, &verified_image, grant)?;
                let (_, sealed) = private_hla_pin(&catalog, &plugin.manifest.id, &req.params)?;
                sealed
            } else {
                catalog.load_sealed_snapshot(requested_sid)?
            };
            dataset_digest = Some(sealed.dataset_digest);
            grant_summary = Some(grant.summary_line());
        }

        plugin.manifest.validate_inputs(&req.params)?;
        let selected_offer = plugin.manifest.resolve_offer(req.offer_id.as_deref())?;
        let canonical_params = canonical_json::to_vec(&req.params)
            .map_err(|e| CompassError::Validation(format!("canonicalize params.json: {e}")))?;
        if (canonical_params.len() as u64) > selected_offer.max_input_bytes {
            return Err(CompassError::Validation(format!(
                "INPUT_LIMIT_EXCEEDED: canonical params.json is {} bytes, exceeding offer '{}' max_input_bytes ({})",
                canonical_params.len(),
                selected_offer.id,
                selected_offer.max_input_bytes
            )));
        }

        let workload_hash = compute_canonical_workload_hash(
            &plugin,
            &verified_image,
            &req.params,
            dataset_digest.as_deref(),
            req.dataset_grant.as_ref(),
        )?;

        let now = Utc::now().timestamp();
        let quote_expires_ts = now + 600;
        let expires_at = Utc
            .timestamp_opt(quote_expires_ts, 0)
            .single()
            .ok_or_else(|| CompassError::Internal("invalid quote expiry timestamp".into()))?;

        let provider_id = self.keys.provider_id();
        let requester_id = self.keys.requester_id();

        let capabilities = if plugin.manifest.network.is_airgapped() {
            Vec::new()
        } else {
            vec!["network.egress.allowlist".to_string()]
        };

        let limits = ExecutionLimits {
            max_input_bytes: selected_offer.max_input_bytes as usize,
            max_runtime_ms: selected_offer.max_runtime_sec.saturating_mul(1_000),
            max_memory_bytes: (selected_offer.max_memory_mb as usize)
                .saturating_mul(1024 * 1024),
            max_output_bytes: selected_offer.max_output_bytes as usize,
            fuel_limit: 0,
        };

        let descriptor = self.sign_provider(
            ARTIFACT_TYPE_DESCRIPTOR,
            now,
            DescriptorPayload {
                provider_id: provider_id.clone(),
                descriptor_seq: 1,
                protocol_version: FROGLET_SCHEMA_V1.into(),
                expires_at: Some(now + 86_400),
                linked_identities: Vec::new(),
                transport_endpoints: Vec::new(),
                capabilities: DescriptorCapabilities {
                    service_kinds: vec![CONTAINER_SERVICE_KIND.into()],
                    execution_runtimes: vec![ExecutionRuntime::Container.as_str().into()],
                    max_concurrent_deals: Some(4),
                },
                accepted_payment_methods: vec![PAYMENT_METHOD_FREE.into()],
            },
        )?;

        let offer = self.sign_provider(
            ARTIFACT_TYPE_OFFER,
            now,
            OfferPayload {
                provider_id: provider_id.clone(),
                offer_id: format!("{}:{}", plugin.manifest.id.as_str(), selected_offer.id),
                descriptor_hash: descriptor.hash.clone(),
                expires_at: Some(now + 3_600),
                offer_kind: CONTAINER_SERVICE_KIND.into(),
                settlement_method: SETTLEMENT_METHOD_NONE.into(),
                quote_ttl_secs: 600,
                execution_profile: OfferExecutionProfile {
                    runtime: ExecutionRuntime::Container,
                    package_kind: "oci_image".into(),
                    contract_version: CONTAINER_ABI_VERSION.into(),
                    access_handles: Vec::new(),
                    abi_version: CONTAINER_ABI_VERSION.into(),
                    capabilities: capabilities.clone(),
                    max_input_bytes: limits.max_input_bytes,
                    max_runtime_ms: limits.max_runtime_ms,
                    max_memory_bytes: limits.max_memory_bytes,
                    max_output_bytes: limits.max_output_bytes,
                    fuel_limit: limits.fuel_limit,
                },
                price_schedule: OfferPriceSchedule {
                    base_fee_msat: 0,
                    success_fee_msat: 0,
                },
                terms_hash: Some(release_terms_hash(&plugin)?),
                confidential_profile_hash: None,
            },
        )?;

        let signed_quote = self.sign_provider(
            ARTIFACT_TYPE_QUOTE,
            now,
            QuotePayload {
                provider_id,
                requester_id,
                descriptor_hash: descriptor.hash.clone(),
                offer_hash: offer.hash.clone(),
                expires_at: quote_expires_ts,
                workload_kind: CONTAINER_SERVICE_KIND.into(),
                workload_hash: workload_hash.clone(),
                confidential_session_hash: None,
                capabilities_granted: capabilities,
                extension_refs: Vec::new(),
                quote_use: None,
                settlement_terms: QuoteSettlementTerms {
                    method: SETTLEMENT_METHOD_NONE.into(),
                    destination_identity: String::new(),
                    base_fee_msat: 0,
                    success_fee_msat: 0,
                    max_base_invoice_expiry_secs: 0,
                    max_success_hold_expiry_secs: 0,
                    min_final_cltv_expiry: 0,
                },
                execution_limits: limits,
            },
        )?;

        let descriptor_json = serde_json::to_value(&descriptor)
            .map_err(|e| CompassError::Internal(format!("serialize descriptor: {e}")))?;
        let offer_json = serde_json::to_value(&offer)
            .map_err(|e| CompassError::Internal(format!("serialize offer: {e}")))?;
        let quote_json = serde_json::to_value(&signed_quote)
            .map_err(|e| CompassError::Internal(format!("serialize quote: {e}")))?;

        let run_id =
            RunId::new(format!("run-{}", Uuid::new_v4().simple())).map_err(CompassError::Internal)?;
        let estimated_cost_act = selected_offer.total_cost_act();
        let quote = RunQuote {
            run_id: run_id.clone(),
            plugin_id: plugin.manifest.id.clone(),
            plugin_version: plugin.manifest.version.clone(),
            image_ref: verified_image.raw.clone(),
            target_node_id: self.node_id.clone(),
            offer_id: selected_offer.id.clone(),
            offer_name: selected_offer.name.clone(),
            base_fee_act: selected_offer.base_fee_act(),
            estimated_cost_act,
            max_input_bytes: selected_offer.max_input_bytes,
            max_output_bytes: selected_offer.max_output_bytes,
            max_runtime_sec: selected_offer.max_runtime_sec,
            max_memory_mb: selected_offer.max_memory_mb,
            workload_hash,
            dataset_grant_summary: grant_summary,
            froglet_quote: quote_json.clone(),
            expires_at,
        };

        let mut initial_logs = Vec::new();
        if let Some(notice) = &execution_notice {
            initial_logs.push(format!("[stage] {notice}"));
        }

        let run_record = RunRecord {
            run_id: run_id.clone(),
            org_id: req.org_id,
            session_id: req.session_id.clone(),
            plugin_id: plugin.manifest.id.clone(),
            plugin_version: plugin.manifest.version.clone(),
            status: RunStatus::Prepared,
            admission: None,
            params: req.params,
            grant: req.dataset_grant,
            quote: quote.clone(),
            result_json: None,
            artifacts: Vec::new(),
            froglet_receipt: None,
            froglet_chain: vec![descriptor_json, offer_json, quote_json],
            receipt_verified: false,
            logs: initial_logs,
            error_message: None,
            created_at: Utc::now(),
            completed_at: None,
        };
        self.session_store.save_run(&run_record).await?;

        if let Some(session_id) = &req.session_id {
            self.session_store
                .append_event(
                    session_id,
                    SessionEventKind::RunPrepared,
                    &format!(
                        "Prepared plugin '{}' ({}) — awaiting explicit user confirmation.",
                        plugin.manifest.id,
                        estimated_cost_act.display_act()
                    ),
                    json!({
                        "run_id": run_id,
                        "plugin_id": plugin.manifest.id,
                        "estimated_cost_act": estimated_cost_act,
                        "workload_hash": quote.workload_hash,
                    }),
                )
                .await?;
        }

        Ok(quote)
    }

    async fn confirm_run(&self, run_id: &RunId, idempotency_key: &str) -> CompassResult<RunRecord> {
        self.confirm_run_guarded(run_id, idempotency_key, None).await
    }

    fn subscribe_logs(&self) -> broadcast::Receiver<RunLogEvent> {
        self.log_tx.subscribe()
    }
}

impl InProcessNodeRuntime {
    pub async fn confirm_run_guarded(&self, run_id: &RunId, idempotency_key: &str,
        revoked: Option<&AtomicBool>) -> CompassResult<RunRecord> {
        self.confirm_run_with_queries(run_id, idempotency_key, revoked, None).await
    }

    pub async fn confirm_run_with_queries(
        &self,
        run_id: &RunId,
        idempotency_key: &str,
        revoked: Option<&AtomicBool>,
        queries: Option<Arc<dyn biocompass_oci_runner::LiveQueryResponder>>,
    ) -> CompassResult<RunRecord> {
        let idempotency_key = idempotency_key.trim();
        if idempotency_key.is_empty() {
            return Err(CompassError::Validation("idempotency_key is required".into()));
        }
        // Only persisted runs need a lock inode. Read the authoritative state again once
        // any confirmation or recovery already in progress has released that inode.
        self.session_store.get_run(run_id).await?;
        let _run_lock = run_lock::acquire(&self.stage_root_dir, run_id).await?;
        let mut run = self.session_store.get_run(run_id).await?;
        if let Some(reconciled) = self.reconcile_confirmation(&run, idempotency_key).await? {
            return Ok(reconciled);
        }
        if run.status != RunStatus::Prepared {
            return Err(CompassError::Validation(format!(
                "run '{}' cannot be confirmed from status {:?}",
                run_id, run.status
            )));
        }
        if Utc::now() > run.quote.expires_at {
            return Err(CompassError::Validation(format!(
                "quote for run '{}' expired at {}",
                run_id, run.quote.expires_at
            )));
        }
        if run.froglet_chain.len() != 3 {
            return Err(CompassError::Protocol(format!(
                "prepared run '{}' must hold [Descriptor, Offer, Quote], found {} artifacts",
                run_id,
                run.froglet_chain.len()
            )));
        }

        let descriptor_doc = run.froglet_chain[0].clone();
        let offer_doc = run.froglet_chain[1].clone();
        let quote_doc = run.froglet_chain[2].clone();
        let signed_quote: SignedArtifact<QuotePayload> = serde_json::from_value(quote_doc.clone())
            .map_err(|e| CompassError::Protocol(format!("decode prepared Quote: {e}")))?;

        let (plugin, _active_release) = self.local_execution_plugin(&run.plugin_id, Some(&run.plugin_version)).await?;
        let requires_live_queries = plugin.manifest.has_live_queries();
        if queries.is_some() != requires_live_queries
            || (requires_live_queries && !run.grant.as_ref().is_some_and(|grant| live_query_grant(&plugin, grant)))
        {
            return Err(CompassError::AccessDenied(
                "reviewed live reads require the corresponding private provider".into(),
            ));
        }
        let signed_offer: SignedArtifact<OfferPayload> = serde_json::from_value(offer_doc.clone())
            .map_err(|e| CompassError::Protocol(format!("decode prepared Offer: {e}")))?;
        if run.quote.plugin_version != run.plugin_version
            || run.quote.plugin_id != run.plugin_id
            || run.quote.run_id != *run_id
            || run.quote.froglet_quote != quote_doc
            || signed_offer.artifact_type != ARTIFACT_TYPE_OFFER
            || signed_quote.artifact_type != ARTIFACT_TYPE_QUOTE
            || signed_offer.signer != self.provider_id()
            || signed_quote.signer != signed_offer.signer
            || signed_quote.payload.offer_hash != signed_offer.hash
            || !froglet_protocol::protocol::verify_artifact(&signed_offer)
            || !froglet_protocol::protocol::verify_artifact(&signed_quote)
        {
            return Err(CompassError::IntegrityViolation(format!("invalid signed offer or quote for run '{run_id}'")));
        }
        // Original native offers lacked terms_hash. All new offers sign it, so removing it
        // from a stored offer invalidates both its signature and the quote's offer hash.
        if let Some(expected) = &signed_offer.payload.terms_hash {
            let selected = plugin.manifest.resolve_offer(Some(&run.quote.offer_id))?;
            if *expected != release_terms_hash(&plugin)?
                || signed_offer.payload.offer_id != format!("{}:{}", run.plugin_id, selected.id)
                || selected.total_cost_act() != run.quote.estimated_cost_act
            {
                return Err(CompassError::IntegrityViolation(format!(
                    "installed module or stored price differs from the signed offer for run '{run_id}'"
                )));
            }
        }
        let verified_image = if plugin.manifest.image.trim() == run.quote.image_ref {
            self.oci_registry.verify_image(&run.quote.image_ref).await?
        } else if self.bundled_dockerfile_dir(&plugin).is_some()
            && run.quote.image_ref.starts_with("local/bundled-")
        {
            self.oci_runner
                .verify_local_bundled_image(&run.quote.image_ref)
                .await?
        } else {
            return Err(CompassError::IntegrityViolation(format!(
                "installed module image differs from the quoted image for run '{run_id}'"
            )));
        };

        // Recompute canonical workload_hash at confirm time before starting container (B6)
        // and pin `(snapshot_id, dataset_digest)` for staging (`SEC-31`).
        let confirm_snapshot_pin: Option<(u64, String)> = if let Some(grant) = &run.grant {
            let catalog = self.get_catalog(&grant.catalog_id).await?;
            let filter_query: QueryAst = run
                .params
                .get("cohort_filter")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| CompassError::Validation(format!("invalid cohort_filter: {e}")))?
                .unwrap_or_default();
            let requested_sid = filter_query.snapshot_id;
            if is_private_catalog(grant) {
                require_reviewed_hla_module(&plugin, &verified_image, grant)?;
                if !self.is_curated_module_approved(
                    &plugin.manifest.id, &plugin.manifest.version, &verified_image.canonical_module_hash()
                ).await {
                    return Err(CompassError::AccessDenied(
                        "reviewed HLA module approval is required for this private release".into(),
                    ));
                }
                let (_, sealed) = private_hla_pin(&catalog, &plugin.manifest.id, &run.params)?;
                Some((sealed.snapshot_id, sealed.dataset_digest))
            } else {
                if requires_live_queries && !self.is_curated_module_approved(
                    &plugin.manifest.id, &plugin.manifest.version, &verified_image.canonical_module_hash()
                ).await {
                    return Err(CompassError::AccessDenied(
                        "reviewed module approval is required for these scoped reads".into(),
                    ));
                }
                let sealed = catalog.load_sealed_snapshot(requested_sid)?;
                Some((sealed.snapshot_id, sealed.dataset_digest))
            }
        } else {
            None
        };
        let confirm_dataset_digest = confirm_snapshot_pin.as_ref().map(|(_, d)| d.clone());
        let recomputed_wh = compute_canonical_workload_hash(
            &plugin,
            &verified_image,
            &run.params,
            confirm_dataset_digest.as_deref(),
            run.grant.as_ref(),
        )?;
        if recomputed_wh != signed_quote.payload.workload_hash
            || recomputed_wh != run.quote.workload_hash
        {
            return Err(CompassError::IntegrityViolation(format!(
                "workload_hash mismatch at confirm: quote='{}', recomputed='{recomputed_wh}'",
                signed_quote.payload.workload_hash
            )));
        }

        let deal_now = Utc::now().timestamp();
        let admission_deadline = (deal_now + 60).min(signed_quote.payload.expires_at);
        let quote_runtime_sec = (signed_quote.payload.execution_limits.max_runtime_ms / 1_000).max(1) as i64;
        let preparation_secs = if verified_image.sha256_digest.is_some() {
            biocompass_core_traits::types::OCI_IMAGE_PREPARATION_TIMEOUT_SECS as i64
        } else {
            0
        };
        let completion_deadline = admission_deadline + preparation_secs + quote_runtime_sec.max(60);
        let acceptance_deadline = completion_deadline + 300;

        let signed_deal = self.sign_requester(
            ARTIFACT_TYPE_DEAL,
            deal_now,
            DealPayload {
                requester_id: self.keys.requester_id(),
                provider_id: self.keys.provider_id(),
                quote_hash: signed_quote.hash.clone(),
                workload_hash: signed_quote.payload.workload_hash.clone(),
                confidential_session_hash: None,
                extension_refs: Vec::new(),
                authority_ref: None,
                supersedes_deal_hash: None,
                client_nonce: Some(idempotency_key.to_owned()),
                success_payment_hash: "00".repeat(32),
                admission_deadline,
                completion_deadline,
                acceptance_deadline,
            },
        )?;
        let deal_doc = serde_json::to_value(&signed_deal)
            .map_err(|e| CompassError::Internal(format!("serialize deal: {e}")))?;

        // Reserve ACT credits BEFORE launching any compute.
        self.payment_gateway
            .reserve_for_run(
                &run.org_id,
                run_id,
                run.quote.estimated_cost_act,
                idempotency_key,
            )
            .await?;

        run.status = RunStatus::Running;
        run.froglet_chain.push(deal_doc.clone());
        self.session_store.save_run(&run).await?;

        self.execute_confirmed_deal(
            run,
            &plugin,
            &verified_image,
            descriptor_doc,
            offer_doc,
            &signed_quote,
            quote_doc,
            &signed_deal,
            deal_doc,
            confirm_snapshot_pin,
            revoked,
            queries,
        )
        .await
    }

}

#[cfg(test)]
mod sec_31_tests {
    use super::*;
    use biocompass_core_traits::{
        CreditAmountAct, LocalFsBlobStore, OciRegistryClient, OrgId, SqliteActLedger,
        SqliteSessionStore,
    };
    use biocompass_data_engine::seed_demo_hla_catalog;
    use biocompass_oci_runner::{DetectedEngine, EngineKind, OciContainerRunner};

    struct LocalTagRegistry;

    #[async_trait]
    impl OciRegistryClient for LocalTagRegistry {
        async fn verify_image(&self, image_raw: &str) -> CompassResult<OciImageRef> {
            OciImageRef::parse(image_raw, true)
        }
    }

    #[tokio::test]
    async fn sec_31_prepare_validates_cohort_filter_and_stage_pins_confirmed_snapshot_digest() {
        let tmp = tempfile::tempdir().unwrap();
        let keys = NodeKeypairSet::load_or_generate(tmp.path().join("keys")).unwrap();
        let session_store = Arc::new(SqliteSessionStore::in_memory().unwrap());
        let blob_store = Arc::new(LocalFsBlobStore::new(tmp.path().join("blobs")).unwrap());
        let ledger =
            Arc::new(SqliteActLedger::in_memory(CreditAmountAct::from_whole_act(100)).unwrap());
        let fake_docker = tmp.path().join("fake-docker");
        std::fs::write(&fake_docker, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_docker, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let oci_runner = Arc::new(OciContainerRunner::from_engine(DetectedEngine {
            kind: EngineKind::Docker,
            binary_path: fake_docker,
            server_version: "test".into(),
            rootless: false,
        }));
        let runtime = InProcessNodeRuntime::new(NodeRuntimeDeps {
            keys,
            session_store: session_store.clone(),
            blob_store,
            oci_registry: Arc::new(LocalTagRegistry),
            payment_gateway: ledger,
            oci_runner,
            stage_root_dir: tmp.path().join("stages"),
            allow_local_dev_tags: true,
        })
        .unwrap();

        let cat_dir = tmp.path().join("catalog");
        seed_demo_hla_catalog(&cat_dir).unwrap();
        let catalog = DuckDbCatalogEngine::open_verified(&cat_dir).unwrap();
        let catalog_id = catalog.manifest().id.clone();
        let v1_snap = catalog.load_sealed_snapshot(None).unwrap();
        runtime.register_catalog(catalog).await.unwrap();

        let plugin_dir = tmp.path().join("plugin");
        biocompass_plugin_spec::write_template_to_dir(
            biocompass_plugin_spec::TemplateKind::PythonUv,
            "alithea/sec31-probe",
            "SEC31 Probe",
            false,
            &plugin_dir,
        )
        .unwrap();
        let loaded = LoadedPlugin::load_from_dir(&plugin_dir, true).unwrap();
        let plugin_id = loaded.manifest.id.clone();
        runtime.register_plugin(loaded.clone()).await.unwrap();

        let org_id = OrgId::new("org-sec31").unwrap();
        let grant = DatasetGrant::scoped_rows_partner(org_id.clone(), catalog_id.clone());

        // 1. Invalid `cohort_filter` MUST fail at `prepare_run` (not deferred to staging).
        let bad_prep = runtime
            .prepare_run(PrepareRunRequest {
                org_id: org_id.clone(),
                session_id: None,
                plugin_id: plugin_id.clone(),
                plugin_version: None,
                offer_id: None,
                params: json!({
                    "alleles": ["HLA-A*02:01"],
                    "min_length": 8,
                    "max_length": 11,
                    "cohort_filter": { "limit": "not-a-number" }
                }),
                dataset_grant: Some(grant.clone()),
            })
            .await;
        assert!(bad_prep.is_err(), "SEC-31: malformed cohort_filter must fail in prepare_run");

        // 2. Valid prepare with unpinned `cohort_filter` captures v1 digest; if catalog advances to v2,
        // `stage_and_run_container` pinned to `(v1_snap.snapshot_id, "wrong-digest")` MUST fail closed!
        let quote = runtime
            .prepare_run(PrepareRunRequest {
                org_id: org_id.clone(),
                session_id: None,
                plugin_id: plugin_id.clone(),
                plugin_version: None,
                offer_id: None,
                params: json!({
                    "alleles": ["HLA-A*02:01"],
                    "min_length": 8,
                    "max_length": 11
                }),
                dataset_grant: Some(grant.clone()),
            })
            .await
            .unwrap();
        let mut run = session_store.get_run(&quote.run_id).await.unwrap();
        let verified_image = OciImageRef::parse(&loaded.manifest.image, true).unwrap();
        let stage_dir = tmp.path().join("stage-mismatch");
        let in_dir = stage_dir.join("in");
        let out_dir = stage_dir.join("out");
        let limits = ExecutionLimits {
            max_runtime_ms: 60_000,
            max_memory_bytes: 512 * 1024 * 1024,
            max_input_bytes: 1024 * 1024,
            max_output_bytes: 1024 * 1024,
            fuel_limit: 0,
        };
        let wrong_pin = (v1_snap.snapshot_id, "0".repeat(64));
        let res = runtime
            .stage_and_run_container(
                &mut run,
                &loaded,
                &verified_image,
                &limits,
                &stage_dir,
                &in_dir,
                &out_dir,
                Some(&wrong_pin),
                None,
                None,
            )
            .await;
        let err = match res {
            Ok(_) => panic!("SEC-31: mismatched pinned snapshot digest must fail closed"),
            Err(err) => err,
        };
        assert_eq!(err.0, "SNAPSHOT_PROVENANCE_MISMATCH");
    }
}
