use super::*;
use biocompass_core_traits::{compute_canonical_workload_hash, result_manifest_hash};
use froglet_verify::DocumentStatus;

fn inconsistent(detail: &str) -> CompassError {
    CompassError::IntegrityViolation(format!("workspace pickup: {detail}"))
}

/// Verify a released hosted run before publishing locally downloaded files.
/// The paired hub supplies the catalog/snapshot identity. The signed workload binds the
/// dataset digest and any snapshot ID explicitly present in the original parameters.
pub fn verify_released_result(
    run: &RunRecord,
    result_bytes: &[u8],
    primary_source: Option<(&CatalogId, u64, &str)>,
) -> CompassResult<()> {
    verify_released_result_commitment(run, &sha256_hex(result_bytes), primary_source)?;
    let result: Value = serde_json::from_slice(result_bytes)
        .map_err(|e| inconsistent(&format!("decode original result.json: {e}")))?;
    if run.result_json.as_ref().unwrap_or(&Value::Null) != &result {
        return Err(inconsistent(
            "result bytes disagree with the released JSON value",
        ));
    }
    Ok(())
}

/// Verify the signed result manifest using an independently measured result.json digest.
/// Use this when only a separately verified artifact will be materialized.
pub fn verify_released_result_commitment(
    run: &RunRecord,
    result_sha256: &str,
    primary_source: Option<(&CatalogId, u64, &str)>,
) -> CompassResult<()> {
    if run.status != RunStatus::Succeeded || !run.receipt_verified {
        return Err(inconsistent("expected a released successful run"));
    }
    if !is_sha256(result_sha256) {
        return Err(inconsistent("invalid result.json digest"));
    }
    let verification = verify_froglet_chain_offline(&run.froglet_chain, None);
    let kinds = [
        ARTIFACT_TYPE_DESCRIPTOR,
        ARTIFACT_TYPE_OFFER,
        ARTIFACT_TYPE_QUOTE,
        ARTIFACT_TYPE_DEAL,
        ARTIFACT_TYPE_RECEIPT,
    ];
    if !verification.valid
        || verification.artifacts.len() != kinds.len()
        || verification
            .artifacts
            .iter()
            .zip(kinds)
            .any(|(a, kind)| a.status != DocumentStatus::Verified || a.artifact_type != kind)
    {
        return Err(inconsistent("Froglet result chain failed verification"));
    }
    let chain = &run.froglet_chain;
    if run.froglet_receipt.as_ref() != chain.last()
        || run.quote.froglet_quote != chain[2]
        || run.run_id != run.quote.run_id
        || run.plugin_id != run.quote.plugin_id
        || run.plugin_version != run.quote.plugin_version
        || chain[1]["payload"]["offer_id"] != format!("{}#{}", run.plugin_id, run.quote.offer_id)
        || chain[2]["payload"]["workload_hash"] != run.quote.workload_hash
    {
        return Err(inconsistent(
            "run or quote metadata differs from its signed chain",
        ));
    }
    let receipt: SignedArtifact<ReceiptPayload> = serde_json::from_value(chain[4].clone())
        .map_err(|e| inconsistent(&format!("decode signed receipt: {e}")))?;
    let payload = &receipt.payload;
    let image_digest = OciImageRef::parse(&run.quote.image_ref, false)?.canonical_module_hash();
    let result_ref = format!("blob://runs/{}/result.json", run.run_id);
    if payload.execution_state != "succeeded"
        || payload.deal_state != "succeeded"
        || payload.result_ref.as_deref() != Some(&result_ref)
        || payload.executor.module_hash.as_deref() != Some(&image_digest)
    {
        return Err(inconsistent(
            "signed result location, execution state or image differs",
        ));
    }
    let dataset_digest = match (&run.grant, primary_source) {
        (Some(grant), Some((catalog, snapshot, digest)))
            if grant.catalog_id == *catalog
                && grant.org_id == run.org_id
                && run
                    .params
                    .pointer("/cohort_filter/snapshot_id")
                    .and_then(Value::as_u64)
                    .is_none_or(|explicit| explicit == snapshot)
                && is_sha256(digest) =>
        {
            Some(digest)
        }
        (None, None) => None,
        _ => {
            return Err(inconsistent(
                "source binding differs from the original catalog grant",
            ));
        }
    };
    // The historical curation bit participates in the quote but has no separate field on RunRecord.
    let workload_matches = |curated| -> CompassResult<bool> {
        Ok(compute_canonical_workload_hash(
            &run.plugin_id,
            &run.plugin_version,
            &image_digest,
            &run.params,
            dataset_digest,
            run.grant.as_ref(),
            curated,
        )? == run.quote.workload_hash)
    };
    if !workload_matches(false)? && !workload_matches(true)? {
        return Err(inconsistent(
            "source digest, grant or module inputs differ from the signed quote",
        ));
    }
    let manifest = result_manifest_hash(result_sha256, &run.artifacts)?;
    if payload.result_hash.as_deref() != Some(&manifest) {
        return Err(inconsistent(
            "result bytes or ordered artifacts differ from the signed manifest",
        ));
    }
    let mut names = HashSet::new();
    for artifact in &run.artifacts {
        if artifact.name.contains(['\\', '\0'])
            || artifact
                .name
                .split('/')
                .any(|p| matches!(p, "" | "." | ".."))
            || !is_sha256(&artifact.sha256)
            || !names.insert(&artifact.name)
            || artifact.blob_key != format!("runs/{}/artifacts/{}", run.run_id, artifact.name)
        {
            return Err(inconsistent("invalid or duplicate run-relative artifact"));
        }
    }
    Ok(())
}

fn is_sha256(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Match a downloaded artifact to the verified signed result manifest.
pub fn verify_released_artifact(
    artifact: &ArtifactRef,
    measured_sha256: &str,
    measured_size: u64,
) -> CompassResult<()> {
    if artifact.size_bytes != measured_size || artifact.sha256 != measured_sha256 {
        return Err(inconsistent(&format!(
            "downloaded artifact '{}' failed its length or digest check",
            artifact.name
        )));
    }
    Ok(())
}
