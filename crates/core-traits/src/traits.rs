//! The 5 cloud-agnostic capability traits decoupling domain crates from deployment backing services.
//!
//! - Local (`biocompass up`): SQLite WAL + POSIX `~/.biocompass/store` + in-process `NodeRuntime` (`settlement_method = "none"`).
//! - Cloud (`compass-hub` + `biocompass --mode cloud-web`): PostgreSQL 16 + S3 REST v4 + OCI Distribution v1.1 + worker pool + PostgreSQL EUR ledger.

use crate::ids::{MilliEur, OrgId, ProjectId, RunId, SessionId};
use crate::types::{
    CompassResult, LedgerTx, OciImageRef, PrepareRunRequest, ProjectRecord, RunLogEvent, RunQuote,
    RunRecord, RunReservation, SessionEvent, SessionEventKind, SessionRecord,
};
use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;
use tokio::sync::broadcast;

/// 1. Durable session, transcript event, and run metadata store (SQLite locally; PostgreSQL 16 in Cloud).
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create_session(
        &self,
        org_id: &OrgId,
        title: &str,
        active_grant_id: &str,
    ) -> CompassResult<SessionRecord>;

    async fn update_session_grant(
        &self,
        session_id: &SessionId,
        active_grant_id: &str,
    ) -> CompassResult<SessionRecord>;

    async fn update_session(
        &self,
        session_id: &SessionId,
        title: Option<&str>,
        active_grant_id: Option<&str>,
        project_id: Option<Option<&ProjectId>>,
    ) -> CompassResult<SessionRecord>;

    async fn delete_session(&self, session_id: &SessionId) -> CompassResult<()>;

    async fn get_session(&self, session_id: &SessionId) -> CompassResult<SessionRecord>;

    async fn list_sessions(&self, org_id: &OrgId) -> CompassResult<Vec<SessionRecord>>;

    async fn create_project(&self, org_id: &OrgId, name: &str) -> CompassResult<ProjectRecord>;

    async fn rename_project(
        &self,
        project_id: &ProjectId,
        name: &str,
    ) -> CompassResult<ProjectRecord>;

    async fn list_projects(&self, org_id: &OrgId) -> CompassResult<Vec<ProjectRecord>>;

    async fn delete_project(&self, project_id: &ProjectId) -> CompassResult<()>;

    async fn append_event(
        &self,
        session_id: &SessionId,
        kind: SessionEventKind,
        content: &str,
        metadata: Value,
    ) -> CompassResult<SessionEvent>;

    async fn list_events(&self, session_id: &SessionId) -> CompassResult<Vec<SessionEvent>>;

    async fn save_run(&self, run: &RunRecord) -> CompassResult<()>;

    /// Persist a terminal run and its optional transcript entry.
    async fn finish_run(
        &self,
        run: &RunRecord,
        summary: &str,
        metadata: Value,
    ) -> CompassResult<()> {
        self.save_run(run).await?;
        if let Some(session_id) = &run.session_id {
            self.append_event(session_id, SessionEventKind::RunCompleted, summary, metadata)
                .await?;
        }
        Ok(())
    }

    async fn get_run(&self, run_id: &RunId) -> CompassResult<RunRecord>;

    async fn list_runs(&self, org_id: &OrgId) -> CompassResult<Vec<RunRecord>>;
}

/// 2. Content-addressed and key-addressed binary artifact & tool-spill store (POSIX locally; S3 REST v4 in Cloud).
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Store raw bytes at a logical `/` key and return its lowercase hex SHA-256 digest.
    async fn put_bytes(&self, key: &str, bytes: &[u8]) -> CompassResult<String>;

    /// Read raw bytes from a logical key.
    async fn get_bytes(&self, key: &str) -> CompassResult<Vec<u8>>;

    /// Read the stored bytes and return their lowercase SHA-256 digest and size.
    /// Remote stores can stream the object to keep artifact verification memory bounded.
    async fn sha256_and_size(&self, key: &str) -> CompassResult<(String, u64)> {
        use sha2::{Digest, Sha256};
        let bytes = self.get_bytes(key).await?;
        Ok((hex::encode(Sha256::digest(&bytes)), bytes.len() as u64))
    }

    /// Check whether a logical key exists.
    async fn exists(&self, key: &str) -> CompassResult<bool>;

    /// Enumerate all object keys strictly below a directory, for historical result recovery.
    async fn list_keys_under(&self, directory: &str) -> CompassResult<Vec<String>> {
        Err(crate::CompassError::Storage(format!(
            "listing blob keys below '{directory}' is unavailable"
        )))
    }

    /// Copy/materialize a stored object into a local scratch path (e.g. `/in` container staging).
    async fn materialize_to_path(&self, key: &str, dest: &Path) -> CompassResult<u64>;
}

/// 3. Standard OCI Distribution Spec v1.1 (`/v2/`) image manifest & digest resolver.
#[async_trait]
pub trait OciRegistryClient: Send + Sync {
    /// Validate an image reference against policy and verify digest reachability if online.
    async fn verify_image(&self, image_raw: &str) -> CompassResult<OciImageRef>;
}

/// 4. Two-step plugin execution dispatcher (in-process `NodeRuntime` locally; WSS `froglet-node` router in Cloud).
#[async_trait]
pub trait RunDispatcher: Send + Sync {
    /// Step 1 (`Prepare`): validate inputs against `plugin.toml` JSON Schema, compute workload hash,
    /// sign a `froglet/v1` `Quote`, and persist a `Prepared` run awaiting user confirmation.
    async fn prepare_run(&self, req: PrepareRunRequest) -> CompassResult<RunQuote>;

    /// Step 2 (`Confirm`): reserve EUR balance (when billed), materialize `/in/data/*.parquet`,
    /// run the hardened container, verify & sign the `froglet/v1` `Receipt`, and settle.
    async fn confirm_run(&self, run_id: &RunId, idempotency_key: &str) -> CompassResult<RunRecord>;

    /// Subscribe to live container stdout/stderr and stage events for a run.
    fn subscribe_logs(&self) -> broadcast::Receiver<RunLogEvent>;
}

/// 5. Pluggable double-entry EUR credit ledger (PostgreSQL + B2B Invoice / card / marketplace in Cloud).
#[async_trait]
pub trait Ledger: Send + Sync {
    /// Inspect the original confirmation terms when this ledger exposes run reservations.
    async fn run_reservation(
        &self,
        _org_id: &OrgId,
        _run_id: &RunId,
    ) -> CompassResult<Option<RunReservation>> {
        Err(crate::types::CompassError::Payment(
            "this ledger does not expose native run reservations".into(),
        ))
    }

    /// Return available (unreserved) EUR balance for `org_id`.
    async fn available_balance(&self, org_id: &OrgId) -> CompassResult<MilliEur>;

    /// Atomically reserve `amount` EUR before dispatching a confirmed `Deal`.
    async fn reserve_for_run(
        &self,
        org_id: &OrgId,
        run_id: &RunId,
        amount: MilliEur,
        idempotency_key: &str,
    ) -> CompassResult<LedgerTx>;

    /// Finalize a reservation after cryptographic verification of a `succeeded` Froglet `Receipt`.
    async fn settle_run(
        &self,
        org_id: &OrgId,
        run_id: &RunId,
        receipt_hash: &str,
    ) -> CompassResult<LedgerTx>;

    /// Refund reserved EUR back to `org_id` if container execution fails, times out, or is canceled.
    async fn refund_run(
        &self,
        org_id: &OrgId,
        run_id: &RunId,
        reason: &str,
    ) -> CompassResult<LedgerTx>;

    /// Credit `org_id` via manual B2B PO/invoice or verified payment webhook.
    async fn credit_topup(
        &self,
        org_id: &OrgId,
        amount: MilliEur,
        reference: &str,
        via_webhook: bool,
    ) -> CompassResult<LedgerTx>;

    /// List immutable ledger transactions for auditability.
    async fn list_transactions(&self, org_id: &OrgId) -> CompassResult<Vec<LedgerTx>>;
}
