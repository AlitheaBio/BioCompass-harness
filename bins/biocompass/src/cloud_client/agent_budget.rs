//! An authenticated execution capability. Only the chat submitter can select the budget.
use super::*;
use biocompass_agent_core::tools::{AdmissionStatus, AgentAdmission};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ExecutionBudget {
    Finite { limit_milli_eur: u64 },
    Unlimited,
}

impl Default for ExecutionBudget {
    fn default() -> Self {
        Self::Finite { limit_milli_eur: 0 }
    }
}

impl ExecutionBudget {
    pub(crate) fn requires_hosted_execution(&self) -> bool {
        !matches!(self, Self::Finite { limit_milli_eur: 0 })
    }
}

pub(crate) struct BudgetedAgentPort {
    hub: Arc<CompassHubClient>,
    execution_id: Uuid,
    enabled: bool,
}

impl BudgetedAgentPort {
    pub(crate) async fn start(
        hub: Arc<CompassHubClient>,
        session: &SessionId,
        budget: &ExecutionBudget,
        requested_id: Option<Uuid>,
    ) -> CompassResult<Arc<dyn AgentExecutionPort>> {
        #[derive(Deserialize)]
        struct Created {
            execution_id: Uuid,
        }

        let mut body = json!({"session_id": session});
        match budget {
            ExecutionBudget::Finite { limit_milli_eur } => {
                body["budget_milli_eur"] =
                    json!(i64::try_from(*limit_milli_eur).map_err(
                        |_| CompassError::Validation("EUR allowance is too large".into())
                    )?);
            }
            ExecutionBudget::Unlimited => body["unlimited"] = json!(true),
        }
        if let Some(id) = requested_id {
            body["execution_id"] = json!(id);
        }
        let created: Created = send_json(
            hub.http
                .post(format!("{}/api/v1/agent-executions", hub.cfg.base_url))
                .json(&body),
            "create agent execution",
        )
        .await?;
        Ok(Arc::new(Self {
            hub,
            execution_id: created.execution_id,
            enabled: budget.requires_hosted_execution(),
        }))
    }

    async fn admit(&self, quote: &RunQuote) -> CompassResult<(AdmissionStatus, String)> {
        let cost = quote.estimated_cost_milli_eur.display_eur();
        if !self.enabled {
            return Ok((
                AdmissionStatus::ApprovalRequired,
                format!("Please approve the {cost} quote to run this module."),
            ));
        }
        let url = format!(
            "{}/api/v1/agent-executions/{}/runs/{}/confirm",
            self.hub.cfg.base_url, self.execution_id, quote.run_id
        );
        let resp = apply_caller_auth(self.hub.http.post(url).json(&json!({})))
            .send()
            .await
            .map_err(|e| CompassError::Internal(format!("submit budgeted run: {e}")))?;
        match resp.status() {
            StatusCode::OK | StatusCode::ACCEPTED => {
                #[derive(Deserialize)]
                struct Submitted {
                    run_id: RunId,
                }
                let submitted: Submitted = resp
                    .json()
                    .await
                    .map_err(|e| CompassError::Internal(format!("decode admitted run: {e}")))?;
                if submitted.run_id != quote.run_id {
                    return Err(CompassError::IntegrityViolation(
                        "admission returned another run".into(),
                    ));
                }
                Ok((
                    AdmissionStatus::Submitted,
                    format!(
                        "Submitted run {} under this execution’s allowance.",
                        quote.run_id
                    ),
                ))
            }
            StatusCode::CONFLICT => {
                #[derive(Deserialize)]
                struct Conflict {
                    code: Option<String>,
                    error: String,
                    remaining_milli_eur: Option<MilliEur>,
                }
                let body: Conflict = resp.json().await.map_err(|e| {
                    CompassError::Internal(format!("decode agent budget conflict: {e}"))
                })?;
                if body.code.as_deref() != Some("agent_budget_exhausted") {
                    return Err(CompassError::IntegrityViolation(body.error));
                }
                let remaining = body.remaining_milli_eur.ok_or_else(|| {
                    CompassError::Internal("budget response omitted remaining EUR".into())
                })?;
                Ok((
                    AdmissionStatus::ApprovalRequired,
                    format!(
                        "This execution has {} remaining. Please approve the {cost} quote to continue.",
                        remaining.display_eur()
                    ),
                ))
            }
            StatusCode::PAYMENT_REQUIRED => {
                #[derive(Deserialize)]
                struct PaymentFailure {
                    #[serde(default)]
                    code: Option<String>,
                    error: String,
                }
                let body: PaymentFailure = resp
                    .json()
                    .await
                    .map_err(|e| CompassError::Internal(format!("decode payment error: {e}")))?;
                let parsed_code = body
                    .code
                    .as_deref()
                    .and_then(biocompass_core_traits::ErrorCode::parse);
                let is_insufficient = parsed_code
                    == Some(biocompass_core_traits::ErrorCode::InsufficientFunds)
                    || (parsed_code.is_none()
                        && body
                            .error
                            .contains(biocompass_core_traits::ERR_INSUFFICIENT_FUNDS));
                if !is_insufficient {
                    return Err(match parsed_code {
                        Some(code) if code.is_domain_specific() => {
                            CompassError::coded(code, body.error)
                        }
                        _ => CompassError::Payment(body.error),
                    });
                }
                Ok((
                    AdmissionStatus::InsufficientCredits,
                    format!(
                        "Your organization needs more credits for the {cost} quote. Ask an admin to top up the organization before continuing."
                    ),
                ))
            }
            _ => Err(map_http_error(resp).await),
        }
    }
}

#[async_trait]
impl AgentExecutionPort for BudgetedAgentPort {
    async fn list_plugins(&self) -> Vec<LoadedPlugin> {
        AgentExecutionPort::list_plugins(self.hub.as_ref()).await
    }

    async fn list_catalog_manifests(&self) -> Vec<CatalogSummary> {
        AgentExecutionPort::list_catalog_manifests(self.hub.as_ref()).await
    }

    async fn query_catalog(
        &self,
        id: &CatalogId,
        grant: &DatasetGrant,
        query: &QueryAst,
    ) -> CompassResult<QueryTableResult> {
        AgentExecutionPort::query_catalog(self.hub.as_ref(), id, grant, query).await
    }

    async fn prepare_plugin_quote(
        &self,
        req: PrepareRunRequest,
    ) -> CompassResult<(RunQuote, Option<AgentAdmission>)> {
        let quote = self
            .hub
            .prepare_bound_quote(req, Some(self.execution_id))
            .await?;
        if !self.enabled && quote.estimated_cost_milli_eur == MilliEur::ZERO {
            // Keep the existing free-quote handoff to the originating client.
            return Ok((quote, None));
        }
        // A reply can be lost after the hub commits. Surface the original run ID and
        // stop this execution, so another model iteration cannot buy a replacement.
        let (status, message) = self.admit(&quote).await.unwrap_or_else(|e| {
            (
                AdmissionStatus::AdmissionUnavailable,
                format!(
                    "Check run {} before trying again. Its admission could not be verified. {}",
                    quote.run_id,
                    e.user_message()
                ),
            )
        });
        let admission = AgentAdmission {
            execution_id: self.execution_id,
            run_id: quote.run_id.clone(),
            status,
            message,
        };
        Ok((quote, Some(admission)))
    }
}
