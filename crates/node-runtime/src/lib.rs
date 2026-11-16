//! `biocompass-node-runtime`: Two-step `Prepare -> Confirm` Froglet v1 execution node runtime.
//!
//! Orchestrates JSON Schema input validation, OCI Distribution v1.1 manifest verification,
//! double-entry ACT credit reservation/settlement/refund, DuckDB Grant-masked `/in/data/*.parquet`
//! materialization, hardened `OciContainerRunner` execution, `BlobStore` stage-out, and
//! RFC 8785 JCS + `secp256k1` BIP340 Schnorr 5-artifact (`Descriptor -> Offer -> Quote -> Deal -> Receipt`)
//! chain signing & verification via `froglet-protocol` and `froglet-verify`.

use async_trait::async_trait;
use biocompass_core_traits::{
    ArtifactRef, BlobStore, CatalogId, CompassError, CompassResult, NodeId, OciImageRef,
    OciRegistryClient, PaymentGateway, PluginId, PrepareRunRequest, RunDispatcher, RunId,
    RunLogEvent, RunQuote, RunRecord, RunStatus, SessionEventKind, SessionStore,
};
use biocompass_data_engine::{CatalogQuery, DuckDbCatalogEngine};
use biocompass_oci_runner::{ContainerRunSpec, OciContainerRunner, prepare_stage_permissions};
use biocompass_plugin_spec::LoadedPlugin;
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
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};
use uuid::Uuid;

const CONTAINER_SERVICE_KIND: &str = "compute.container.v1";
const CONTAINER_ABI_VERSION: &str = "biocompass.oci.io.v1";
const MAX_STAGE_IO_BYTES: usize = 1_073_741_824; // 1 GiB

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
    plugins: RwLock<HashMap<PluginId, LoadedPlugin>>,
    catalogs: RwLock<HashMap<CatalogId, DuckDbCatalogEngine>>,
    stage_root_dir: PathBuf,
    allow_local_dev_tags: bool,
    log_tx: broadcast::Sender<RunLogEvent>,
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
            oci_runner: deps.oci_runner,
            plugins: RwLock::new(HashMap::new()),
            catalogs: RwLock::new(HashMap::new()),
            stage_root_dir: deps.stage_root_dir,
            allow_local_dev_tags: deps.allow_local_dev_tags,
            log_tx,
        })
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

    /// Register or replace a `LoadedPlugin`.
    pub async fn register_plugin(&self, plugin: LoadedPlugin) -> CompassResult<()> {
        plugin.manifest.validate(self.allow_local_dev_tags)?;
        self.plugins
            .write()
            .await
            .insert(plugin.manifest.id.clone(), plugin);
        Ok(())
    }

    /// List all registered plugins sorted deterministically by `PluginId`.
    pub async fn list_plugins(&self) -> Vec<LoadedPlugin> {
        let map = self.plugins.read().await;
        let mut items: Vec<LoadedPlugin> = map.values().cloned().collect();
        items.sort_by(|a, b| a.manifest.id.as_str().cmp(b.manifest.id.as_str()));
        items
    }

    /// Fetch a registered plugin by ID.
    pub async fn get_plugin(&self, id: &PluginId) -> CompassResult<LoadedPlugin> {
        self.plugins
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| CompassError::NotFound(format!("plugin '{id}' is not registered")))
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
            .stage_and_run_container(&mut run, plugin, verified_image, &stage_dir, &in_dir, &out_dir)
            .await;
        let finished_at = Utc::now().timestamp().max(started_at);

        let capabilities = quote.payload.capabilities_granted.clone();
        let execution_mode = if plugin.manifest.network.is_airgapped() {
            "airgapped".to_string()
        } else {
            "egress_allowlist".to_string()
        };

        let (status, result_json, artifacts, engine_version, receipt_payload, err_opt) =
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
                        success.engine_version,
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
                        engine_ver,
                        payload,
                        Some(message),
                    )
                }
            };

        let receipt = match self.sign_provider(ARTIFACT_TYPE_RECEIPT, finished_at, receipt_payload)
        {
            Ok(r) => r,
            Err(sign_err) => {
                let _ = self
                    .payment_gateway
                    .refund_run(&run.org_id, &run_id, &format!("receipt sign error: {sign_err}"))
                    .await;
                return Err(sign_err);
            }
        };

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
            let _ = self
                .payment_gateway
                .refund_run(&run.org_id, &run_id, &reason)
                .await;
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

        match status {
            RunStatus::Succeeded => {
                self.emit_stage_log(
                    &run_id,
                    format!(
                        "Verified froglet/v1 Schnorr receipt {} (engine={engine_version}); settling ACT reservation",
                        &receipt.hash[..12]
                    ),
                );
                if let Err(settle_err) = self
                    .payment_gateway
                    .settle_run(&run.org_id, &run_id, &receipt.hash)
                    .await
                {
                    let _ = self
                        .payment_gateway
                        .refund_run(
                            &run.org_id,
                            &run_id,
                            &format!("settle failed: {settle_err}"),
                        )
                        .await;
                    return Err(settle_err);
                }
            }
            _ => {
                let refund_reason = err_opt
                    .clone()
                    .unwrap_or_else(|| "container execution failed".into());
                self.emit_stage_log(
                    &run_id,
                    format!("Run failed ({refund_reason}); refunding reserved ACT credits"),
                );
                self.payment_gateway
                    .refund_run(&run.org_id, &run_id, &refund_reason)
                    .await?;
            }
        }

        run.status = status;
        run.result_json = result_json;
        run.artifacts = artifacts;
        run.froglet_receipt = Some(receipt_doc);
        run.froglet_chain = chain_docs;
        run.receipt_verified = true;
        run.error_message = err_opt;
        run.completed_at = Some(Utc::now());

        self.session_store.save_run(&run).await?;
        if let Some(session_id) = &run.session_id {
            let summary = match run.status {
                RunStatus::Succeeded => format!(
                    "Plugin '{}' completed with verified Froglet receipt `{}`.",
                    run.plugin_id,
                    &receipt.hash[..16]
                ),
                _ => format!(
                    "Plugin '{}' failed ({}); reserved ACT refunded.",
                    run.plugin_id,
                    run.error_message.as_deref().unwrap_or("unknown error")
                ),
            };
            self.session_store
                .append_event(
                    session_id,
                    SessionEventKind::RunCompleted,
                    &summary,
                    json!({
                        "run_id": run.run_id,
                        "status": run.status,
                        "receipt_hash": receipt.hash,
                    }),
                )
                .await?;
        }

        Ok(run)
    }

    async fn stage_and_run_container(
        &self,
        run: &mut RunRecord,
        plugin: &LoadedPlugin,
        verified_image: &OciImageRef,
        stage_dir: &Path,
        in_dir: &Path,
        out_dir: &Path,
    ) -> Result<StageSuccessOutcome, (String, String, String)> {
        let default_engine_ver = format!(
            "{}:{}",
            self.oci_runner.engine().kind.as_str(),
            self.oci_runner.engine().server_version
        );

        prepare_stage_permissions(stage_dir, in_dir, out_dir)
            .map_err(|e| ("STAGE_PREPARE_ERROR".into(), e.to_string(), default_engine_ver.clone()))?;

        // 1. Write canonical /in/params.json (world-readable inside 0700 stage_dir for UID 65532).
        let params_bytes = canonical_json::to_vec(&run.params).map_err(|e| {
            (
                "PARAMS_ENCODE_ERROR".into(),
                format!("canonicalize params.json: {e}"),
                default_engine_ver.clone(),
            )
        })?;
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

        // 2. Materialize pre-masked DuckDB Parquet slice into /in/data/cohort.parquet when a Grant is attached.
        if let Some(grant) = &run.grant {
            let catalog = self.get_catalog(&grant.catalog_id).await.map_err(|e| {
                (
                    "CATALOG_LOOKUP_ERROR".into(),
                    e.to_string(),
                    default_engine_ver.clone(),
                )
            })?;
            let filter_query: CatalogQuery = run
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
                .materialize_scoped_parquet(grant, &filter_query, &cohort_parquet)
                .map_err(|e| {
                    (
                        "DUCKDB_STAGE_ERROR".into(),
                        e.to_string(),
                        default_engine_ver.clone(),
                    )
                })?;
            set_world_readable_file(&cohort_parquet).map_err(|e| {
                (
                    "STAGE_PERMS_ERROR".into(),
                    e.to_string(),
                    default_engine_ver.clone(),
                )
            })?;
            let stage_msg = format!(
                "Staged DuckDB cohort slice ({} rows, masked={}, sha256={})",
                slice.row_count,
                slice.sequence_masked,
                &slice.sha256[..12]
            );
            run.logs.push(format!("[stage] {stage_msg}"));
            self.emit_stage_log(&run.run_id, stage_msg);
        }

        // 3. Execute hardened OCI container.
        self.emit_stage_log(
            &run.run_id,
            format!("Launching hardened OCI container {}", verified_image.raw),
        );
        let spec = ContainerRunSpec {
            run_id: run.run_id.clone(),
            image: verified_image.clone(),
            in_dir: in_dir.to_path_buf(),
            out_dir: out_dir.to_path_buf(),
            resources: plugin.manifest.resources.clone(),
            network: plugin.manifest.network.clone(),
            command_override: None,
        };

        let report = self
            .oci_runner
            .execute(&spec, Some(self.log_tx.clone()))
            .await
            .map_err(|e| {
                run.logs.push(format!("[error] {e}"));
                (
                    "CONTAINER_EXECUTION_ERROR".into(),
                    e.to_string(),
                    default_engine_ver.clone(),
                )
            })?;

        let engine_version = format!("{}:{}", report.engine.as_str(), report.engine_version);
        for line in &report.stdout_lines {
            run.logs.push(format!("[stdout] {line}"));
        }
        for line in &report.stderr_lines {
            run.logs.push(format!("[stderr] {line}"));
        }

        if report.exit_code != 0 {
            let tail = report
                .stderr_lines
                .last()
                .or_else(|| report.stdout_lines.last())
                .cloned()
                .unwrap_or_else(|| "no stderr output".into());
            return Err((
                format!("EXIT_CODE_{}", report.exit_code),
                format!(
                    "container exited with non-zero code {}: {tail}",
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

        // Canonical result_hash commits to both /out/result.json and all /out/artifacts/* digests!
        let canonical_result_manifest = canonical_json::to_vec(&json!({
            "result_json_sha256": result_sha256,
            "artifacts": artifacts.iter().map(|a| json!({
                "name": a.name,
                "sha256": a.sha256,
                "size_bytes": a.size_bytes,
            })).collect::<Vec<_>>(),
        }))
        .map_err(|e| {
            (
                "RESULT_HASH_ERROR".into(),
                format!("canonicalize result manifest: {e}"),
                engine_version.clone(),
            )
        })?;
        let result_hash = sha256_hex(&canonical_result_manifest);

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

#[async_trait]
impl RunDispatcher for InProcessNodeRuntime {
    async fn prepare_run(&self, req: PrepareRunRequest) -> CompassResult<RunQuote> {
        let plugin = self.get_plugin(&req.plugin_id).await?;
        plugin.manifest.validate_inputs(&req.params)?;

        let verified_image = self
            .oci_registry
            .verify_image(&plugin.manifest.image)
            .await?;

        let mut dataset_digest: Option<String> = None;
        let mut grant_summary: Option<String> = None;
        if let Some(grant) = &req.dataset_grant {
            if grant.org_id != req.org_id {
                return Err(CompassError::AccessDenied(format!(
                    "dataset grant '{}' belongs to org '{}', not '{}'",
                    grant.grant_id, grant.org_id, req.org_id
                )));
            }
            let catalog = self.get_catalog(&grant.catalog_id).await?;
            catalog.verify_integrity()?;
            dataset_digest = Some(catalog.manifest().dataset_digest());
            grant_summary = Some(format!(
                "{} (catalog={}, sequence_masked={}, max_rows={})",
                grant.label, grant.catalog_id, grant.mask_peptide_sequences, grant.max_rows
            ));
        }

        let workload_descriptor = json!({
            "plugin_id": plugin.manifest.id,
            "plugin_version": plugin.manifest.version,
            "image_digest": verified_image.canonical_module_hash(),
            "params": req.params,
            "dataset_digest": dataset_digest,
            "grant_id": req.dataset_grant.as_ref().map(|g| &g.grant_id),
            "sequence_masked": req.dataset_grant.as_ref().map(|g| g.mask_peptide_sequences),
        });
        let workload_bytes = canonical_json::to_vec(&workload_descriptor)
            .map_err(|e| CompassError::Protocol(format!("canonicalize workload: {e}")))?;
        let workload_hash = crypto::sha256_hex(&workload_bytes);

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
            max_input_bytes: MAX_STAGE_IO_BYTES,
            max_runtime_ms: plugin.manifest.resources.timeout_sec.saturating_mul(1_000),
            max_memory_bytes: (plugin.manifest.resources.memory_mb as usize)
                .saturating_mul(1024 * 1024),
            max_output_bytes: MAX_STAGE_IO_BYTES,
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
                offer_id: plugin.manifest.id.as_str().to_owned(),
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
                terms_hash: None,
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
        let estimated_cost_act = plugin.manifest.estimated_cost_act();
        let quote = RunQuote {
            run_id: run_id.clone(),
            plugin_id: plugin.manifest.id.clone(),
            plugin_version: plugin.manifest.version.clone(),
            image_ref: verified_image.raw.clone(),
            target_node_id: self.node_id.clone(),
            estimated_cost_act,
            workload_hash,
            dataset_grant_summary: grant_summary,
            froglet_quote: quote_json.clone(),
            expires_at,
        };

        let run_record = RunRecord {
            run_id: run_id.clone(),
            org_id: req.org_id,
            session_id: req.session_id.clone(),
            plugin_id: plugin.manifest.id.clone(),
            plugin_version: plugin.manifest.version.clone(),
            status: RunStatus::Prepared,
            params: req.params,
            grant: req.dataset_grant,
            quote: quote.clone(),
            result_json: None,
            artifacts: Vec::new(),
            froglet_receipt: None,
            froglet_chain: vec![descriptor_json, offer_json, quote_json],
            receipt_verified: false,
            logs: Vec::new(),
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
        let mut run = self.session_store.get_run(run_id).await?;
        if run.status == RunStatus::Succeeded {
            return Ok(run);
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

        let plugin = self.get_plugin(&run.plugin_id).await?;
        let verified_image = self.oci_registry.verify_image(&run.quote.image_ref).await?;

        // Reserve ACT credits BEFORE launching any compute.
        self.payment_gateway
            .reserve_for_run(
                &run.org_id,
                run_id,
                run.quote.estimated_cost_act,
                idempotency_key,
            )
            .await?;

        let deal_now = Utc::now().timestamp();
        let admission_deadline = (deal_now + 60).min(signed_quote.payload.expires_at);
        let completion_deadline =
            admission_deadline + (plugin.manifest.resources.timeout_sec as i64).max(60);
        let acceptance_deadline = completion_deadline + 300;

        let signed_deal = match self.sign_requester(
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
        ) {
            Ok(d) => d,
            Err(e) => {
                let _ = self
                    .payment_gateway
                    .refund_run(&run.org_id, run_id, &format!("deal sign failed: {e}"))
                    .await;
                return Err(e);
            }
        };

        let deal_doc = match serde_json::to_value(&signed_deal) {
            Ok(v) => v,
            Err(e) => {
                let _ = self
                    .payment_gateway
                    .refund_run(&run.org_id, run_id, "deal serialize failed")
                    .await;
                return Err(CompassError::Internal(format!("serialize deal: {e}")));
            }
        };

        run.status = RunStatus::Running;
        if let Err(save_err) = self.session_store.save_run(&run).await {
            let _ = self
                .payment_gateway
                .refund_run(&run.org_id, run_id, "save running state failed")
                .await;
            return Err(save_err);
        }

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
        )
        .await
    }

    fn subscribe_logs(&self) -> broadcast::Receiver<RunLogEvent> {
        self.log_tx.subscribe()
    }
}
