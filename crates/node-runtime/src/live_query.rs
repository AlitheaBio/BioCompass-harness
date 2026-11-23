//! Bounded, privately audited reads for a trusted provider and a pinned module run.
use crate::target_validation::{NamedReadPage, RuntimeReadParams, runtime_read_page_on_catalog};
use async_trait::async_trait;
use biocompass_core_traits::{CompassError, CompassResult, DatasetGrant, RunId, RunRecord, ScopedQueryDefinition};
use biocompass_data_engine::{
    DuckLakeCatalogEngine,
    scoped_join::ScopeRelation,
};
use biocompass_oci_runner::LiveQueryResponder;
use chrono::{DateTime, Utc};
use froglet_protocol::crypto::sha256_hex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;

const CALL_LIMIT: usize = 64;
const REPLY_LIMIT: usize = 1024 * 1024;
const SESSION_LIMIT: usize = 8 * REPLY_LIMIT;

/// Host-owned authorization and private audit persistence for a single run.
#[async_trait]
pub trait LiveQueryAuthority: Send + Sync {
    async fn ensure_allowed(&self) -> CompassResult<()>;
    async fn save_audit(&self, document: &[u8]) -> CompassResult<()>;
}

pub struct LiveQueryBinding {
    pub catalog: DuckLakeCatalogEngine,
    pub version: u64,
    pub snapshot: u64,
    pub digest: String,
}

pub struct LiveQuerySession {
    authority: Arc<dyn LiveQueryAuthority>,
    binding: LiveQueryBinding,
    run_id: RunId,
    grant: DatasetGrant,
    plugin_id: String,
    plugin_version: String,
    workload_hash: String,
    declared: Vec<String>,
    join: Option<(ScopeRelation, BTreeMap<String, ScopedQueryDefinition>)>,
    state: Mutex<Calls>,
}

struct Calls {
    entries: Vec<Entry>,
    bytes_left: usize,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    request_id: String,
    query: String,
    params: serde_json::Value,
}

#[derive(Serialize)]
struct Entry {
    sequence: usize,
    #[serde(flatten)]
    request: Request,
    started_at: DateTime<Utc>,
    completed_at: DateTime<Utc>,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_sha256: Option<String>,
}

impl LiveQuerySession {
    pub fn new(
        run: &RunRecord,
        binding: LiveQueryBinding,
        declared: Vec<String>,
        input_left: usize,
        authority: Arc<dyn LiveQueryAuthority>,
    ) -> CompassResult<Self> {
        let grant = run.grant.clone().ok_or_else(|| {
            CompassError::IntegrityViolation("missing live run dataset grant".into())
        })?;
        if binding.catalog.summary().id != grant.catalog_id {
            return Err(CompassError::IntegrityViolation(
                "live query dataset does not match its grant".into(),
            ));
        }
        Ok(Self {
            authority,
            binding,
            run_id: run.run_id.clone(),
            grant,
            plugin_id: run.plugin_id.to_string(),
            plugin_version: run.plugin_version.clone(),
            workload_hash: run.quote.workload_hash.clone(),
            declared,
            join: None,
            state: Mutex::new(Calls { entries: Vec::new(), bytes_left: input_left.min(SESSION_LIMIT) }),
        })
    }

    pub fn with_scoped_joins(mut self, relation: ScopeRelation, queries: BTreeMap<String, ScopedQueryDefinition>) -> Self {
        self.declared = queries.keys().cloned().collect();
        self.join = Some((relation, queries));
        self
    }

    pub async fn ensure_allowed(&self) -> CompassResult<()> {
        self.authority.ensure_allowed().await
    }
}

#[async_trait]
impl LiveQueryResponder for LiveQuerySession {
    async fn call(&self, frame: &[u8]) -> CompassResult<Vec<u8>> {
        let request: Request = serde_json::from_slice(frame)
            .map_err(|_| CompassError::Validation("invalid live query request".into()))?;
        let mut calls = self.state.lock().await;
        if request.request_id.is_empty()
            || request.request_id.len() > 64
            || !request.request_id.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')
            })
            || !self.declared.contains(&request.query)
            || calls.entries.len() >= CALL_LIMIT
            || calls.entries.iter().any(|entry| entry.request.request_id == request.request_id)
        {
            return Err(CompassError::Validation("invalid or undeclared live query request".into()));
        }
        let started_at = Utc::now();
        let outcome = async {
            self.ensure_allowed().await?;
            let engine = self.binding.catalog.clone();
            let snapshot = self.binding.snapshot;
            let digest = self.binding.digest.clone();
            let query = request.query.clone();
            let params = request.params.clone();
            let grant = self.grant.clone();
            let join = self.join.clone();
            let page = tokio::task::spawn_blocking(move || {
                let invalid = |_| CompassError::Validation("invalid typed query parameters".into());
                if let Some((relation, definitions)) = join {
                    let definition = definitions.get(&query).ok_or_else(|| {
                        CompassError::Validation("undeclared scoped join".into())
                    })?;
                    let value = relation.execute_query(definition, &engine, snapshot, &digest, &grant, params)?;
                    Ok(NamedReadPage { rows: value.into_iter().map(|row| json!(row)).collect(), next_after_sample_id: None })
                } else {
                    let parameter: RuntimeReadParams = serde_json::from_value(params).map_err(invalid)?;
                    runtime_read_page_on_catalog(&engine, snapshot, &digest, grant.row_scope.as_ref(), &query, &parameter)
                }
            }).await.map_err(|_| CompassError::Internal("live query task failed".into()))??;
            self.ensure_allowed().await?;
            let mut response = json!(page);
            response["request_id"] = json!(request.request_id);
            let bytes = serde_json::to_vec(&response)
                .map_err(|_| CompassError::Internal("live query response encoding failed".into()))?;
            if bytes.len() > REPLY_LIMIT || bytes.len() > calls.bytes_left {
                return Err(CompassError::Validation("live query response exceeds input allowance".into()));
            }
            calls.bytes_left -= bytes.len();
            Ok(bytes)
        }.await;
        let state = match &outcome {
            Ok(_) => "succeeded",
            Err(CompassError::AccessDenied(_)) => "denied",
            Err(_) => "failed",
        };
        let sequence = calls.entries.len() + 1;
        calls.entries.push(Entry {
            sequence, request, started_at, completed_at: Utc::now(), state,
            response_sha256: outcome.as_ref().ok().map(|bytes| sha256_hex(bytes)),
        });
        let document = serde_json::to_vec(&json!({
            "run_id": self.run_id, "plugin_id": self.plugin_id, "plugin_version": self.plugin_version,
            "workload_hash": self.workload_hash, "dataset_version": self.binding.version, "snapshot_id": self.binding.snapshot,
            "dataset_digest": self.binding.digest, "grant_id": self.grant.grant_id,
            "scope_sha256": self.grant.scope_digest(), "calls": calls.entries,
        })).map_err(|_| CompassError::Storage("encode private live query audit".into()))?;
        self.authority.save_audit(&document).await?;
        let response = outcome?;
        self.ensure_allowed().await?;
        Ok(response)
    }
}
