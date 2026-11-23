//! The original publisher attests the node permitted to modify this local family.
//! For older native imports that publisher was the requester's Froglet key.
use super::{local, store_error};
use crate::dataset_store::{LocalStore, State as Family};
use crate::state::AppState;
use biocompass_core_traits::{CompassError, CompassResult};
use biocompass_node_runtime::InProcessNodeRuntime;
use serde_json::{Value, json};

fn denied() -> CompassError {
    CompassError::AccessDenied("this private release belongs to a different publisher".into())
}

fn identity(family: &Family, provider: &str) -> CompassResult<Value> {
    let original = family.version(Some(1)).map_err(store_error)?;
    let manifest = &original.manifest;
    if manifest["format"] != "biocompass.private-dataset.v1"
        || manifest["catalog_id"] != format!("bc-private-v1/{}", family.dataset)
        || manifest["visibility"] != "private"
        || manifest["source_sha256"] != family.source_sha
        || !manifest["validation"]["importer"]["manifest_sha256"].is_string()
        || manifest["validation"]["importer"]["image"] != family.importer
    {
        return Err(denied());
    }
    Ok(json!({
        "org":family.org, "dataset":family.dataset, "provider":provider,
        "original_publisher": original.actor, "source_sha256":family.source_sha,
        "original_dataset_digest":manifest["dataset_digest"]
    }))
}

pub(super) fn verified_provider(family: &Family) -> CompassResult<&str> {
    let claim = family.node_owner.as_ref().ok_or_else(denied)?;
    let provider = claim["payload"]["provider"].as_str().ok_or_else(denied)?;
    let original = family.version(Some(1)).map_err(store_error)?;
    InProcessNodeRuntime::verify_dataset_owner(
        &original.actor,
        &identity(family, provider)?,
        claim,
    )?;
    Ok(provider)
}

pub(crate) fn owner(app: &AppState, store: &LocalStore, dataset: &str) -> CompassResult<String> {
    let runtime = local(app)?;
    bind(&runtime, store, dataset, app.default_org_id.as_str())
}

pub(super) fn bind(
    runtime: &InProcessNodeRuntime,
    store: &LocalStore,
    dataset: &str,
    org: &str,
) -> CompassResult<String> {
    let mut family = store.published_state(dataset).map_err(store_error)?;
    if family.dataset != dataset || family.org != org {
        return Err(denied());
    }
    let provider = runtime.provider_id();
    if family.node_owner.is_none() {
        let first = family.version(Some(1)).map_err(store_error)?;
        let claim = runtime.sign_dataset_owner(&first.actor, identity(&family, &provider)?)?;
        family = store
            .bind_node_owner(dataset, org, &first.actor, &provider, claim)
            .map_err(store_error)?;
    }
    if verified_provider(&family)? != provider {
        return Err(denied());
    }
    Ok(provider)
}

pub(crate) fn sign_release_change(state: &AppState, payload: Value) -> CompassResult<Value> {
    local(state)?.sign_dataset_action(payload)
}

pub(crate) fn verify_release_change(
    state: &AppState,
    payload: &Value,
    proof: &Value,
) -> CompassResult<()> {
    InProcessNodeRuntime::verify_dataset_action(&local(state)?.provider_id(), payload, proof)
}
