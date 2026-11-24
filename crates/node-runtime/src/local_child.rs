//! Laptop child-run broker (`W7`, `N-1`): a notebook (or any module declaring `child_modules`)
//! running in the in-process runtime starts child modules that run locally, as the local user,
//! with `settlement_method = "none"`. Each child gets its own Quote -> Deal -> Receipt chain
//! linked to the parent with `bc.parent.v1`; the parent's receipt lists the child with
//! `bc.child.v1`.
//!
//! Authority: the local user may run every locally installed module and read every local
//! catalog, so `"caller"` admits any installed module and an explicit list admits only listed
//! modules (checked by the mailbox before the broker is called). `grant_id` / `catalog_id`
//! choose among the local user's grants; they never widen access. Depth stays one.
//! Spending: the sum of child quotes is bounded by the parent's signed child spend cap.

use super::InProcessNodeRuntime;
use async_trait::async_trait;
use biocompass_core_traits::{
    CatalogId, CompassError, CompassResult, DatasetGrant, OrgId, PrepareRunRequest,
    RunDispatcher, RunId, RunStatus, SessionId,
};
use biocompass_oci_runner::{
    ChildModuleRequest, ChildRunBroker, ChildRunBrokerContext, ChildRunReply,
    ChildRunStagedArtifact, child_artifact_alias, stage_child_artifact_bytes,
};
use sha2::{Digest, Sha256};
use std::sync::Weak;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

pub(crate) struct LocalChildBroker {
    pub(crate) runtime: Weak<InProcessNodeRuntime>,
    pub(crate) org_id: OrgId,
    pub(crate) session_id: Option<SessionId>,
    pub(crate) parent_run_id: RunId,
    /// Signed per-parent cap on the summed child quotes (milli-EUR).
    pub(crate) spend_cap_milli_eur: i64,
    pub(crate) spent_milli_eur: AtomicI64,
}

impl LocalChildBroker {
    /// Local user's grants: full, partner and compute-only presets on every local catalog.
    async fn resolve_local_grant(
        runtime: &InProcessNodeRuntime,
        org_id: &OrgId,
        grant_id: Option<&str>,
        catalog_id: Option<&str>,
    ) -> CompassResult<Option<DatasetGrant>> {
        let grant_id = grant_id.map(str::trim).filter(|s| !s.is_empty());
        let catalog_id = catalog_id.map(str::trim).filter(|s| !s.is_empty());
        if grant_id.is_none() && catalog_id.is_none() {
            return Ok(None);
        }
        let mut grants = Vec::new();
        for cat in runtime.list_catalogs().await {
            let cid = cat.manifest().id.clone();
            if catalog_id.is_some_and(|wanted| wanted != cid.as_str()) {
                continue;
            }
            grants.push(DatasetGrant::full_access(org_id.clone(), cid.clone()));
            grants.push(DatasetGrant::scoped_rows_partner(org_id.clone(), cid.clone()));
            grants.push(DatasetGrant::compute_only_partner(org_id.clone(), cid));
        }
        let found = match grant_id {
            None => grants.into_iter().next(),
            Some("full") => grants.into_iter().find(|g| g.grant_id.starts_with("grant-full-")),
            Some("partner") => grants.into_iter().find(|g| g.grant_id.starts_with("grant-partner-")),
            Some("compute_only" | "compute-only") => grants
                .into_iter()
                .find(|g| g.grant_id.starts_with("grant-compute-only-")),
            Some(exact) => grants.into_iter().find(|g| g.grant_id == exact),
        };
        found.map(Some).ok_or_else(|| {
            CompassError::AccessDenied(format!(
                "FORBIDDEN: no local grant matches grant_id {grant_id:?} on catalog {:?}",
                catalog_id.map(CatalogId::from)
            ))
        })
    }
}

#[async_trait]
impl ChildRunBroker for LocalChildBroker {
    async fn run_child(
        &self,
        ctx: &ChildRunBrokerContext,
        request: ChildModuleRequest,
    ) -> CompassResult<ChildRunReply> {
        let Some(runtime) = self.runtime.upgrade() else {
            return Ok(ChildRunReply::error(
                request.request_id,
                "HUB_UNAVAILABLE",
                "local runtime is shutting down",
            ));
        };
        let grant = match Self::resolve_local_grant(
            &runtime,
            &self.org_id,
            request.grant_id.as_deref(),
            request.catalog_id.as_deref(),
        )
        .await
        {
            Ok(grant) => grant,
            Err(err) => return Ok(ChildRunReply::error(request.request_id, err.code(), err.to_string())),
        };
        let quote = match runtime
            .prepare_run(PrepareRunRequest {
                org_id: self.org_id.clone(),
                session_id: self.session_id.clone(),
                plugin_id: request.plugin_id.clone(),
                plugin_version: request.plugin_version.clone(),
                offer_id: request.offer_id.clone(),
                params: request.params.clone(),
                dataset_grant: grant,
                inputs: request.inputs.clone(),
                parent_run_id: Some(self.parent_run_id.as_str().to_owned()),
                agent_credential_id: None,
                agent_tool_call_id: Some(format!("child:{}", request.request_id)),
                doc_id: None,
                doc_version: None,
            })
            .await
        {
            Ok(quote) => quote,
            Err(err) => return Ok(ChildRunReply::error(request.request_id, err.code(), err.to_string())),
        };
        let cost = quote.estimated_cost_milli_eur.as_milli();
        let spent = self.spent_milli_eur.load(Ordering::Acquire);
        if spent.saturating_add(cost) > self.spend_cap_milli_eur {
            return Ok(ChildRunReply::error(
                request.request_id,
                "CHILD_SPEND_CAP_EXCEEDED",
                format!(
                    "child would bring this run's child spend to {} milli-EUR, above its signed cap of {}",
                    spent.saturating_add(cost),
                    self.spend_cap_milli_eur
                ),
            ));
        }
        self.spent_milli_eur.fetch_add(cost, Ordering::AcqRel);

        let wait_secs = request
            .timeout_sec
            .map(|t| t.min(ctx.remaining_child_wait_sec))
            .unwrap_or(ctx.remaining_child_wait_sec)
            .max(1);
        let child_run_id = quote.run_id.clone();
        let idem = format!("child-{}-{}", self.parent_run_id.as_str(), request.request_id);
        let child_runtime = runtime.clone();
        let task_run_id = child_run_id.clone();
        // The child keeps its own lifecycle; a timeout only stops the parent waiting for it.
        let task = tokio::spawn(async move { child_runtime.confirm_run(&task_run_id, &idem).await });
        let finished = match tokio::time::timeout(Duration::from_secs(wait_secs), task).await {
            Ok(Ok(Ok(run))) => run,
            Ok(Ok(Err(err))) => {
                return Ok(ChildRunReply {
                    run_id: Some(child_run_id.as_str().to_owned()),
                    ..ChildRunReply::error(request.request_id, err.code(), err.to_string())
                });
            }
            Ok(Err(join)) => {
                return Ok(ChildRunReply::error(
                    request.request_id,
                    "CHILD_RUN_FAILED",
                    format!("child task failed: {join}"),
                ));
            }
            Err(_) => {
                return Ok(ChildRunReply {
                    run_id: Some(child_run_id.as_str().to_owned()),
                    ..ChildRunReply::error(
                        request.request_id,
                        "CHILD_RUN_TIMEOUT",
                        format!("child run did not complete within {wait_secs}s"),
                    )
                });
            }
        };
        if finished.status != RunStatus::Succeeded {
            // Failed children settle at most their base fee; release the unused cap share.
            self.spent_milli_eur.fetch_sub(cost, Ordering::AcqRel);
        }

        let total: u64 = finished.artifacts.iter().map(|a| a.size_bytes).sum();
        if ctx.remaining_child_artifact_bytes.is_some_and(|left| total > left) {
            return Ok(ChildRunReply {
                run_id: Some(finished.run_id.as_str().to_owned()),
                status: Some(finished.status),
                receipt_hash: receipt_hash_of(&finished),
                ..ChildRunReply::error(
                    request.request_id,
                    "INPUT_LIMIT_EXCEEDED",
                    format!("child artifacts ({total} bytes) exceed the parent's remaining input allowance"),
                )
            });
        }
        let alias = child_artifact_alias(&request.request_id);
        let mut staged = Vec::with_capacity(finished.artifacts.len());
        for art in &finished.artifacts {
            let bytes = runtime.blob_store.get_bytes(&art.blob_key).await?;
            let actual = hex::encode(Sha256::digest(&bytes));
            crate::verify_released_artifact(art, &actual, bytes.len() as u64)?;
            let path = stage_child_artifact_bytes(&ctx.parent_in_dir, &alias, &art.name, &bytes)?;
            staged.push(ChildRunStagedArtifact {
                name: art.name.clone(),
                sha256: art.sha256.clone(),
                size_bytes: art.size_bytes,
                media_type: art.media_type.clone(),
                staged_path: Some(path),
            });
        }
        Ok(ChildRunReply {
            request_id: request.request_id,
            ok: finished.status == RunStatus::Succeeded,
            status: Some(finished.status),
            run_id: Some(finished.run_id.as_str().to_owned()),
            receipt_hash: receipt_hash_of(&finished),
            result_json: finished.result_json,
            result_path: None,
            artifacts: staged,
            error_code: None,
            error: finished.error_message,
        })
    }
}

fn receipt_hash_of(run: &biocompass_core_traits::RunRecord) -> Option<String> {
    run.froglet_receipt
        .as_ref()
        .and_then(|r| r.get("hash"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}
