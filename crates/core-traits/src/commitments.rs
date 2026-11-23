//! Signed output and hosted workload commitments shared by producers and pickup clients.
use crate::{ArtifactRef, CompassError, CompassResult, DatasetGrant, PluginId};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn canonical_digest(document: &Value) -> CompassResult<String> {
    let bytes = serde_json_canonicalizer::to_vec(document)
        .map_err(|error| CompassError::Protocol(format!("canonicalize commitment: {error}")))?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}

/// Froglet's result commitment for a BioCompass run uses the exact stored result.json
/// bytes and the name, digest and length of each artifact, in worker manifest order.
pub fn result_manifest_hash(
    result_sha256: &str,
    artifacts: &[ArtifactRef],
) -> CompassResult<String> {
    let entries: Vec<_> = artifacts
        .iter()
        .map(|artifact| {
            json!({
                "name": artifact.name,
                "sha256": artifact.sha256,
                "size_bytes": artifact.size_bytes,
            })
        })
        .collect();
    let manifest = json!({"result_json_sha256": result_sha256, "artifacts": entries});
    canonical_digest(&manifest)
}

/// Compute the deterministic RFC 8785 JCS SHA-256 `workload_hash` over every execution-critical input (`B6`).
/// Both `compass-hub` and `froglet-node` recompute this independently and compare it to the signed `Quote`
/// before reserving ACT or launching any container.
pub fn compute_canonical_workload_hash(
    plugin_id: &PluginId,
    plugin_version: &str,
    image_digest: &str,
    params: &Value,
    dataset_digest: Option<&str>,
    effective_grant: Option<&DatasetGrant>,
    is_curated_approved: bool,
) -> CompassResult<String> {
    let workload_descriptor = json!({
        "plugin_id": plugin_id,
        "plugin_version": plugin_version,
        "image_digest": image_digest,
        "params": params,
        "dataset_digest": dataset_digest,
        "grant": effective_grant,
        "is_curated_approved": is_curated_approved,
    });
    canonical_digest(&workload_descriptor)
}
