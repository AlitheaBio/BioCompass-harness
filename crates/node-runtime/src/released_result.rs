use super::*;
use biocompass_core_traits::{
    ParsedExtensionRef, compute_canonical_workload_hash, parse_extension_ref, result_manifest_hash,
};
use froglet_verify::DocumentStatus;
use serde::{Deserialize, Serialize};

fn inconsistent(detail: &str) -> CompassError {
    CompassError::IntegrityViolation(format!("workspace pickup: {detail}"))
}

/// What the brokering party (the hub, or the local session store) recorded for a run's
/// parent/child links (`W6-2`). Pickup compares the signed receipt to this, never to itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokeredRunLinks {
    /// Deal hash of the parent run the broker started this run for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_deal_hash: Option<String>,
    /// Receipt hashes of child runs the broker started for this run.
    #[serde(default)]
    pub child_receipt_hashes: Vec<String>,
}

impl BrokeredRunLinks {
    /// Derive the brokered links of `run_id` from a hub-exported `bc.provenance.v1` bundle: the
    /// hub enumerates children from its own run table (`parent_run_id`), not from the receipt.
    pub fn from_provenance_bundle(bundle: &Value, run_id: &str, parent_run_id: Option<&str>) -> Self {
        let runs = bundle
            .get("runs")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let chain_hash = |run: &Value, idx: usize| {
            run.get("froglet_chain")
                .and_then(|c| c.get(idx))
                .and_then(|d| d.get("hash"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let parent_deal_hash = parent_run_id.and_then(|pid| {
            runs.iter()
                .find(|r| r.get("run_id").and_then(Value::as_str) == Some(pid))
                .and_then(|r| chain_hash(r, 3))
        });
        let mut child_receipt_hashes: Vec<String> = runs
            .iter()
            .filter(|r| r.get("parent_run_id").and_then(Value::as_str) == Some(run_id))
            .filter_map(|r| chain_hash(r, 4))
            .collect();
        child_receipt_hashes.sort();
        child_receipt_hashes.dedup();
        Self {
            parent_deal_hash,
            child_receipt_hashes,
        }
    }

    /// `true` when pickup of `run` needs the broker's view (it has a parent or signs children).
    pub fn required_for(run: &RunRecord) -> bool {
        run.parent_run_id.is_some()
            || run
                .froglet_receipt
                .as_ref()
                .and_then(|r| r.pointer("/payload/extension_refs"))
                .and_then(Value::as_array)
                .is_some_and(|refs| !refs.is_empty())
    }
}

/// How the signed Receipt's `bc.child.v1` refs are checked.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ExpectedChildren<'a> {
    /// Every signed ref must be a child receipt the broker actually started (no fabricated
    /// children). A brokered child the parent abandoned may legitimately be absent.
    Brokered(&'a [String]),
}

/// Fail-closed verification of Quote, Deal, and Receipt `extension_refs` (`W6`, `W7`).
pub(crate) fn verify_signed_chain_extension_refs(
    quote: &SignedArtifact<QuotePayload>,
    deal: &SignedArtifact<DealPayload>,
    receipt: &SignedArtifact<ReceiptPayload>,
    inputs: &[biocompass_core_traits::RunInput],
    has_parent_run_id: bool,
    expected_parent_deal_hash: Option<&str>,
    expected_children: Option<ExpectedChildren<'_>>,
) -> CompassResult<()> {
    let mut observed_parents = Vec::new();
    for raw in &quote.payload.extension_refs {
        match parse_extension_ref(raw)? {
            ParsedExtensionRef::Input { .. } => {}
            ParsedExtensionRef::Parent { parent_deal_hash } => {
                observed_parents.push(parent_deal_hash);
            }
            ParsedExtensionRef::Child { .. } => {
                return Err(CompassError::IntegrityViolation(
                    "bc.child.v1 extension_ref is not permitted on Quote".into(),
                ));
            }
        }
    }
    if observed_parents.len() > 1 {
        return Err(CompassError::IntegrityViolation(
            "Quote carries multiple bc.parent.v1 extension_refs".into(),
        ));
    }
    if let Some(expected) = expected_parent_deal_hash
        && observed_parents.iter().any(|p| p != expected)
    {
        return Err(CompassError::IntegrityViolation(format!(
            "signed bc.parent.v1 does not name the brokered parent deal '{expected}'"
        )));
    }
    let parent_deal_hash = match (has_parent_run_id, expected_parent_deal_hash) {
        (false, Some(_)) => {
            return Err(CompassError::IntegrityViolation(
                "broker reports a parent for a run without parent_run_id".into(),
            ));
        }
        (true, Some(expected)) => Some(expected),
        (true, None) => {
            let Some(found) = observed_parents.first().map(String::as_str) else {
                return Err(CompassError::IntegrityViolation(
                    "child run with parent_run_id is missing bc.parent.v1 on signed Quote".into(),
                ));
            };
            Some(found)
        }
        (false, None) => {
            if !observed_parents.is_empty() {
                return Err(CompassError::IntegrityViolation(
                    "non-child run unexpectedly carries bc.parent.v1 on signed Quote".into(),
                ));
            }
            None
        }
    };

    verify_extension_refs(
        ExtensionRefStage::QuoteOrDeal,
        &quote.payload.extension_refs,
        inputs,
        parent_deal_hash,
        &[],
    )?;
    verify_extension_refs(
        ExtensionRefStage::QuoteOrDeal,
        &deal.payload.extension_refs,
        inputs,
        parent_deal_hash,
        &[],
    )?;

    let mut parsed_children = Vec::with_capacity(receipt.payload.extension_refs.len());
    for raw in &receipt.payload.extension_refs {
        match parse_extension_ref(raw)? {
            ParsedExtensionRef::Child { child_receipt_hash } => {
                parsed_children.push(child_receipt_hash);
            }
            other => {
                return Err(CompassError::IntegrityViolation(format!(
                    "unexpected non-child extension_ref on Receipt: {other:?}"
                )));
            }
        }
    }
    let child_hashes: Vec<String> = match expected_children {
        Some(ExpectedChildren::Brokered(brokered)) => {
            if let Some(unknown) = parsed_children.iter().find(|h| !brokered.contains(h)) {
                return Err(CompassError::IntegrityViolation(format!(
                    "Receipt signs bc.child.v1 '{unknown}' that the broker never started"
                )));
            }
            parsed_children
        }
        None => parsed_children,
    };
    verify_extension_refs(
        ExtensionRefStage::Receipt,
        &receipt.payload.extension_refs,
        &[],
        None,
        &child_hashes,
    )?;
    Ok(())
}

/// Verify a released hosted run before publishing locally downloaded files.
/// The paired hub supplies the catalog/snapshot identity. The signed workload binds the
/// dataset digest and any snapshot ID explicitly present in the original parameters.
pub fn verify_released_result(
    run: &RunRecord,
    result_bytes: &[u8],
    primary_source: Option<(&CatalogId, u64, &str)>,
) -> CompassResult<()> {
    verify_released_result_inner(run, result_bytes, primary_source, None)
}

/// [`verify_released_result`] that also checks the signed parent/child links against what the
/// broker (hub) recorded (`W6-2`). Use this wherever the receipt crosses a trust boundary.
pub fn verify_released_result_with_links(
    run: &RunRecord,
    result_bytes: &[u8],
    primary_source: Option<(&CatalogId, u64, &str)>,
    links: &BrokeredRunLinks,
) -> CompassResult<()> {
    verify_released_result_inner(run, result_bytes, primary_source, Some(links))
}

fn verify_released_result_inner(
    run: &RunRecord,
    result_bytes: &[u8],
    primary_source: Option<(&CatalogId, u64, &str)>,
    links: Option<&BrokeredRunLinks>,
) -> CompassResult<()> {
    verify_released_result_commitment_inner(run, &sha256_hex(result_bytes), primary_source, links)?;
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
///
/// Without brokered links this only checks the receipt's links are well formed; callers that
/// already compared them to their own broker records (the hub at completion) may use it.
pub fn verify_released_result_commitment(
    run: &RunRecord,
    result_sha256: &str,
    primary_source: Option<(&CatalogId, u64, &str)>,
) -> CompassResult<()> {
    verify_released_result_commitment_inner(run, result_sha256, primary_source, None)
}

/// [`verify_released_result_commitment`] with the broker's parent/child records (`W6-2`).
pub fn verify_released_result_commitment_with_links(
    run: &RunRecord,
    result_sha256: &str,
    primary_source: Option<(&CatalogId, u64, &str)>,
    links: &BrokeredRunLinks,
) -> CompassResult<()> {
    verify_released_result_commitment_inner(run, result_sha256, primary_source, Some(links))
}

fn verify_released_result_commitment_inner(
    run: &RunRecord,
    result_sha256: &str,
    primary_source: Option<(&CatalogId, u64, &str)>,
    links: Option<&BrokeredRunLinks>,
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
    let quote: SignedArtifact<QuotePayload> = serde_json::from_value(chain[2].clone())
        .map_err(|e| inconsistent(&format!("decode signed quote: {e}")))?;
    let deal: SignedArtifact<DealPayload> = serde_json::from_value(chain[3].clone())
        .map_err(|e| inconsistent(&format!("decode signed deal: {e}")))?;
    let receipt: SignedArtifact<ReceiptPayload> = serde_json::from_value(chain[4].clone())
        .map_err(|e| inconsistent(&format!("decode signed receipt: {e}")))?;
    verify_signed_chain_extension_refs(
        &quote,
        &deal,
        &receipt,
        &run.inputs,
        run.parent_run_id.is_some(),
        links.and_then(|l| l.parent_deal_hash.as_deref()),
        links.map(|l| ExpectedChildren::Brokered(&l.child_receipt_hashes)),
    )
    .map_err(|e| inconsistent(&e.to_string()))?;
    if let Some(l) = links
        && run.parent_run_id.is_some()
        && l.parent_deal_hash.is_none()
    {
        return Err(inconsistent(
            "the broker did not report the parent deal of this child run",
        ));
    }
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
            &run.inputs,
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

#[cfg(test)]
mod w6_2_tests {
    use super::*;

    fn fixture_chain() -> (
        SignedArtifact<QuotePayload>,
        SignedArtifact<DealPayload>,
        SignedArtifact<ReceiptPayload>,
    ) {
        let value: Value =
            serde_json::from_str(include_str!("../tests/fixtures/released_run.json")).unwrap();
        let chain = &value["froglet_chain"];
        (
            serde_json::from_value(chain[2].clone()).unwrap(),
            serde_json::from_value(chain[3].clone()).unwrap(),
            serde_json::from_value(chain[4].clone()).unwrap(),
        )
    }

    #[test]
    fn signed_child_ref_the_broker_never_started_is_refused() {
        let (quote, deal, mut receipt) = fixture_chain();
        let child = "ab".repeat(32);
        receipt.payload.extension_refs = vec![format!("bc.child.v1:{child}")];
        // The old pickup path (no broker view) only checks the refs against themselves.
        verify_signed_chain_extension_refs(&quote, &deal, &receipt, &[], false, None, None).unwrap();
        // With the broker's record, a fabricated child is refused ...
        let err = verify_signed_chain_extension_refs(
            &quote,
            &deal,
            &receipt,
            &[],
            false,
            None,
            Some(ExpectedChildren::Brokered(&[])),
        )
        .unwrap_err();
        assert!(err.to_string().contains("never started"), "{err}");
        // ... and a brokered one is accepted.
        verify_signed_chain_extension_refs(
            &quote,
            &deal,
            &receipt,
            &[],
            false,
            None,
            Some(ExpectedChildren::Brokered(std::slice::from_ref(&child))),
        )
        .unwrap();
    }

    #[test]
    fn signed_parent_ref_must_name_the_brokered_parent_deal() {
        let (mut quote, mut deal, receipt) = fixture_chain();
        let signed_parent = "cd".repeat(32);
        let parent_ref = format!("bc.parent.v1:{signed_parent}");
        quote.payload.extension_refs = vec![parent_ref.clone()];
        deal.payload.extension_refs = vec![parent_ref];
        verify_signed_chain_extension_refs(&quote, &deal, &receipt, &[], true, None, None).unwrap();
        let other = "ef".repeat(32);
        assert!(
            verify_signed_chain_extension_refs(
                &quote,
                &deal,
                &receipt,
                &[],
                true,
                Some(&other),
                Some(ExpectedChildren::Brokered(&[])),
            )
            .is_err()
        );
        verify_signed_chain_extension_refs(
            &quote,
            &deal,
            &receipt,
            &[],
            true,
            Some(&signed_parent),
            Some(ExpectedChildren::Brokered(&[])),
        )
        .unwrap();
        // A broker-reported parent for a run that has none is inconsistent.
        let (q, d, r) = fixture_chain();
        assert!(
            verify_signed_chain_extension_refs(&q, &d, &r, &[], false, Some(&other), None).is_err()
        );
    }

    #[test]
    fn brokered_links_come_from_the_hubs_run_table_view() {
        let bundle = serde_json::json!({
            "schema": "bc.provenance.v1",
            "runs": [
                {"run_id": "p", "froglet_chain": [{}, {}, {}, {"hash": "d1"}, {"hash": "r1"}]},
                {"run_id": "c1", "parent_run_id": "x", "froglet_chain": [{}, {}, {}, {}, {"hash": "rc1"}]},
                {"run_id": "c2", "parent_run_id": "x", "froglet_chain": [{}, {}, {}]},
                {"run_id": "x", "parent_run_id": "p", "froglet_chain": [{}, {}, {}, {"hash": "dx"}, {"hash": "rx"}]}
            ]
        });
        let links = BrokeredRunLinks::from_provenance_bundle(&bundle, "x", Some("p"));
        assert_eq!(links.parent_deal_hash.as_deref(), Some("d1"));
        assert_eq!(links.child_receipt_hashes, vec!["rc1".to_owned()]);
        let none = BrokeredRunLinks::from_provenance_bundle(&bundle, "x", Some("missing"));
        assert!(none.parent_deal_hash.is_none());
    }
}
