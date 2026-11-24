//! Recovery runs under the same process-shared lease as confirmation. The signed
//! chain is published after the output blobs and before the terminal run record is saved.
use super::*;

impl InProcessNodeRuntime {
    /// The engine retains the workspace label until removal. A failed sweep is safe
    /// to retry on startup and does not hold up recovery or publish outputs.
    pub async fn recover_abandoned_containers(&self) -> CompassResult<()> {
        for container in self.oci_runner.native_job_containers().await? {
            let run_id = container.run_id();
            let run = match self.session_store.get_run(run_id).await {
                Ok(run) => run,
                Err(CompassError::NotFound(_)) => continue,
                Err(error) => return Err(error),
            };
            if !matches!(run.status, RunStatus::Running | RunStatus::Succeeded | RunStatus::Failed) {
                continue;
            }
            let Some(_owner) = run_lock::try_acquire(&self.stage_root_dir, run_id)? else {
                continue;
            };
            self.oci_runner.remove_abandoned_container(&container).await?;
            tracing::info!(%run_id, container_id = %container.engine_id(), "recovered an abandoned native container");
        }
        Ok(())
    }

    /// Reconcile an abandoned confirmation on startup. Another process may still
    /// own the run (for example a CLI sharing the workspace with the server).
    pub async fn recover_interrupted_run(&self, run_id: &RunId) -> CompassResult<()> {
        let Some(_owner) = run_lock::try_acquire(&self.stage_root_dir, run_id)? else {
            return Ok(());
        };
        let run = self.session_store.get_run(run_id).await?;
        if matches!(run.status, RunStatus::Prepared | RunStatus::Running) {
            self.reconcile_run(&run, None).await?;
        }
        Ok(())
    }

    /// Called only while holding the confirmation lease.
    pub(super) async fn reconcile_confirmation(
        &self,
        run: &RunRecord,
        key: &str,
    ) -> CompassResult<Option<RunRecord>> {
        self.reconcile_run(run, Some(key)).await
    }

    async fn reconcile_run(
        &self,
        run: &RunRecord,
        key: Option<&str>,
    ) -> CompassResult<Option<RunRecord>> {
        if run.status == RunStatus::Canceled {
            return Err(CompassError::Validation(format!(
                "run {} was canceled",
                run.run_id
            )));
        }
        if run.status == RunStatus::Prepared {
            return Ok(None);
        }
        let recorded_key = run
            .froglet_chain
            .get(3)
            .and_then(|deal| deal.pointer("/payload/client_nonce"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(requested_key) = key
            && recorded_key.is_some_and(|orig| orig != requested_key)
        {
            return Err(CompassError::IntegrityViolation(format!(
                "confirmation key differs from the original approval for {}",
                run.run_id
            )));
        }
        if matches!(run.status, RunStatus::Succeeded | RunStatus::Failed) {
            if key.is_some() && recorded_key.is_none() {
                return Err(CompassError::IntegrityViolation(format!(
                    "terminal run {} lacks a signed deal confirmation key",
                    run.run_id
                )));
            }
            return Ok(Some(run.clone()));
        }
        let outcome = match recorded_key {
            Some(orig_key) => recovered_result::load_signed_outcome(self, run, orig_key).await?,
            None => None,
        };
        let terminal = match outcome {
            Some(signed) => signed,
            None => {
                let mut interrupted = run.clone();
                interrupted.status = RunStatus::Failed;
                interrupted.result_json = None;
                interrupted.artifacts.clear();
                interrupted.froglet_receipt = None;
                interrupted.receipt_verified = false;
                interrupted.error_message = Some(
                    "Native execution was interrupted before a signed outcome was saved.".into(),
                );
                interrupted.completed_at = Some(Utc::now());
                interrupted
            }
        };
        self.post_and_finish(terminal).await.map(Some)
    }

    /// The signed chain and outputs are already durable for signed outcomes.
    /// A storage error leaves the public record pending so recovery can replay it.
    pub(super) async fn post_and_finish(&self, run: RunRecord) -> CompassResult<RunRecord> {
        let receipt_hash = run
            .froglet_receipt
            .as_ref()
            .and_then(|doc| doc.get("hash"))
            .and_then(Value::as_str);
        let summary = match run.status {
            RunStatus::Succeeded => {
                let hash = verified_receipt_hash(&run)?;
                format!(
                    "Plugin '{}' completed with verified Froglet receipt `{}`.",
                    run.plugin_id,
                    hash.get(..16).unwrap_or(hash)
                )
            }
            RunStatus::Failed => {
                let reason = run
                    .error_message
                    .as_deref()
                    .unwrap_or("container execution failed");
                format!("Plugin '{}' failed ({reason}).", run.plugin_id)
            }
            _ => {
                return Err(CompassError::Internal(
                    "cannot post a nonterminal outcome".into(),
                ));
            }
        };
        self.emit_stage_log(&run.run_id, &summary);
        self.session_store
            .finish_run(
                &run,
                &summary,
                json!({
                    "run_id": run.run_id, "status": run.status, "receipt_hash": receipt_hash
                }),
            )
            .await?;
        let _ = self.terminal_tx.send(biocompass_core_traits::run_events::TerminalNotice {
            org_id: run.org_id.clone(), run_id: run.run_id.clone(), status: run.status,
        });
        Ok(run)
    }
}

fn verified_receipt_hash(run: &RunRecord) -> CompassResult<&str> {
    run.froglet_receipt
        .as_ref()
        .filter(|_| run.receipt_verified)
        .and_then(|doc| doc.get("hash"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CompassError::IntegrityViolation(format!("run {} lacks a verified receipt", run.run_id))
        })
}
