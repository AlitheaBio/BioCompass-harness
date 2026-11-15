//! Core domain records, error taxonomy, OCI image references, and data grants.

use crate::ids::{CatalogId, CreditAmountAct, NodeId, OrgId, PluginId, RunId, SessionId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Structured error type shared across all BioCompass domain crates.
#[derive(Debug, Error)]
pub enum CompassError {
    #[error("resource not found: {0}")]
    NotFound(String),
    #[error("validation error: {0}")]
    Validation(String),
    #[error("access denied by grant policy: {0}")]
    AccessDenied(String),
    #[error("cryptographic or SHA-256 integrity violation: {0}")]
    IntegrityViolation(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("container engine error: {0}")]
    ContainerEngine(String),
    #[error("froglet protocol error: {0}")]
    Protocol(String),
    #[error("payment or ACT ledger error: {0}")]
    Payment(String),
    #[error("upstream LLM provider error: {0}")]
    UpstreamLlm(String),
    #[error("internal error: {0}")]
    Internal(String),
}

/// Result alias for BioCompass operations.
pub type CompassResult<T> = Result<T, CompassError>;

/// Validated OCI container image reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciImageRef {
    /// Full image reference string.
    pub raw: String,
    /// Repository coordinate without digest/tag (e.g. `registry.alithea.bio/alithea/hla-binding-net`).
    pub repository: String,
    /// Optional immutable SHA-256 manifest digest (`64` lowercase hex chars).
    pub sha256_digest: Option<String>,
    /// Optional mutable local development tag (permitted only when local dev tags are explicitly enabled).
    pub local_tag: Option<String>,
}

impl OciImageRef {
    /// Parse an OCI image reference, requiring `@sha256:<64-hex>` unless `allow_local_tag` is `true`
    /// and the image uses the `local/` namespace.
    pub fn parse(raw: &str, allow_local_tag: bool) -> CompassResult<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(CompassError::Validation("OCI image reference cannot be empty".into()));
        }
        if let Some((repo, digest_part)) = trimmed.split_once("@sha256:") {
            if repo.is_empty() {
                return Err(CompassError::Validation(
                    "OCI image repository before @sha256: cannot be empty".into(),
                ));
            }
            let is_valid_hex = digest_part.len() == 64
                && digest_part
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if !is_valid_hex {
                return Err(CompassError::Validation(format!(
                    "OCI image digest must be 64 lowercase hex chars, got '{digest_part}'"
                )));
            }
            return Ok(Self {
                raw: trimmed.to_owned(),
                repository: repo.to_owned(),
                sha256_digest: Some(digest_part.to_owned()),
                local_tag: None,
            });
        }

        if allow_local_tag && trimmed.starts_with("local/") {
            let (repo, tag) = trimmed.split_once(':').unwrap_or((trimmed, "dev"));
            return Ok(Self {
                raw: trimmed.to_owned(),
                repository: repo.to_owned(),
                sha256_digest: None,
                local_tag: Some(tag.to_owned()),
            });
        }

        Err(CompassError::Validation(format!(
            "OCI image '{trimmed}' must be pinned by immutable '@sha256:<64-hex>' digest"
        )))
    }

    /// Return the 64-char hex digest used in Froglet receipts (or deterministic SHA-256 of local dev ref).
    pub fn canonical_module_hash(&self) -> String {
        if let Some(digest) = &self.sha256_digest {
            digest.clone()
        } else {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(self.raw.as_bytes()))
        }
    }
}

/// Row-level and column-level scientific dataset access grant compiled into DuckDB SQL views.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetGrant {
    pub grant_id: String,
    pub label: String,
    pub org_id: OrgId,
    pub catalog_id: CatalogId,
    /// Explicit organ allowlist (`None` = all organs permitted).
    pub allowed_organs: Option<Vec<String>>,
    /// Explicit sample condition allowlist (`None` = all conditions permitted).
    pub allowed_conditions: Option<Vec<String>>,
    /// When `true`, peptide amino-acid sequences are replaced with `'***'` in SQL projection
    /// and filtering/grouping on `sequence` is forbidden to prevent oracle side-channels.
    pub mask_peptide_sequences: bool,
    /// Hard ceiling on rows returned or staged into `/in/data/`.
    pub max_rows: usize,
}

impl DatasetGrant {
    /// Construct a full unmasked grant for local single-tenant scientist mode or dataset owners.
    pub fn full_access(org_id: OrgId, catalog_id: CatalogId) -> Self {
        Self {
            grant_id: format!("grant-full-{}", catalog_id.as_str()),
            label: "Full Research Access (Unmasked All-Tissue Cohort)".into(),
            org_id,
            catalog_id,
            allowed_organs: None,
            allowed_conditions: None,
            mask_peptide_sequences: false,
            max_rows: 100_000,
        }
    }

    /// Construct a partner-restricted grant demonstrating organ row-scoping and `***` sequence masking.
    pub fn restricted_partner_demo(org_id: OrgId, catalog_id: CatalogId) -> Self {
        Self {
            grant_id: format!("grant-partner-lung-masked-{}", catalog_id.as_str()),
            label: "Partner Evaluation Tier (Lung & Tumor Only · Sequences Masked '***')".into(),
            org_id,
            catalog_id,
            allowed_organs: Some(vec!["Lung".into()]),
            allowed_conditions: Some(vec!["NSCLC_Tumor".into(), "Healthy_Adjacent".into()]),
            mask_peptide_sequences: true,
            max_rows: 10_000,
        }
    }
}

/// Durable agent conversation session summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: SessionId,
    pub org_id: OrgId,
    pub title: String,
    pub active_grant_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Discriminant for durable session log events (`Model-visible <=> Logged`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventKind {
    UserMessage,
    AssistantMessage,
    ToolCall,
    ToolResult,
    RunPrepared,
    RunCompleted,
    SystemNotice,
}

/// Single immutable event appended to a durable `SessionRecord`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub event_id: String,
    pub session_id: SessionId,
    pub seq: i64,
    pub kind: SessionEventKind,
    pub content: String,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
}

/// Lifecycle status of a plugin execution run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Prepared,
    Running,
    Succeeded,
    Failed,
    Canceled,
}

/// Request to prepare and quote a plugin container execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareRunRequest {
    pub org_id: OrgId,
    pub session_id: Option<SessionId>,
    pub plugin_id: PluginId,
    pub params: Value,
    pub dataset_grant: Option<DatasetGrant>,
}

/// Pre-execution quote returned before user consent (`Prepare -> Confirm` two-step gate).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunQuote {
    pub run_id: RunId,
    pub plugin_id: PluginId,
    pub plugin_version: String,
    pub image_ref: String,
    pub target_node_id: NodeId,
    pub estimated_cost_act: CreditAmountAct,
    pub workload_hash: String,
    pub dataset_grant_summary: Option<String>,
    pub froglet_quote: Value,
    pub expires_at: DateTime<Utc>,
}

/// Persisted execution record including outputs and signed Froglet v1 artifacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: RunId,
    pub org_id: OrgId,
    pub session_id: Option<SessionId>,
    pub plugin_id: PluginId,
    pub plugin_version: String,
    pub status: RunStatus,
    pub params: Value,
    pub grant: Option<DatasetGrant>,
    pub quote: RunQuote,
    pub result_json: Option<Value>,
    pub artifacts: Vec<ArtifactRef>,
    /// Signed `froglet/v1` `Receipt` JSON verified offline via `froglet-verify`.
    pub froglet_receipt: Option<Value>,
    /// Full 5-artifact `froglet/v1` chain (`Descriptor`, `Offer`, `Quote`, `Deal`, `Receipt`).
    pub froglet_chain: Vec<Value>,
    pub receipt_verified: bool,
    pub logs: Vec<String>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Output file artifact produced in `/out/artifacts/*` and stored in `BlobStore`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub name: String,
    pub blob_key: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub sha256: String,
}

/// Streaming container or stage lifecycle line emitted over SSE.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunLogEvent {
    pub run_id: RunId,
    pub timestamp: DateTime<Utc>,
    pub stream: String,
    pub line: String,
}

/// Double-entry ACT credit ledger transaction kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerTxKind {
    GenesisGrant,
    InvoiceTopUp,
    WebhookTopUp,
    RunReserve,
    RunSettle,
    RunRefund,
}

/// Immutable ACT ledger entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerTx {
    pub tx_id: String,
    pub org_id: OrgId,
    pub run_id: Option<RunId>,
    pub kind: LedgerTxKind,
    pub delta_act: CreditAmountAct,
    pub balance_after_act: CreditAmountAct,
    pub reference: String,
    pub created_at: DateTime<Utc>,
}
