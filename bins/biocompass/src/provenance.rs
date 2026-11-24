//! Offline verification for `bc.provenance.v1` multi-run provenance bundles (`W6`, `W7`).
//!
//! Verifies every run's 5-document `froglet/v1` chain (`Descriptor -> Offer -> Quote -> Deal -> Receipt`),
//! `manifest_preimage` commitment against `receipt.payload.result_hash`, canonical `extension_refs`
//! (`bc.input.v1`, `bc.parent.v1`, `bc.child.v1`), cross-run input artifact & bidirectional
//! parent/child bindings, absence of private `params`, and DAG acyclicity, reporting any
//! omitted upstream/parent/child runs in `unresolved_ancestors`.

use biocompass_core_traits::{
    ArtifactRef, ExtensionRefStage, ParsedExtensionRef, RunInput, is_sha256_hex,
    parse_extension_ref, result_manifest_hash, verify_extension_refs,
};
use biocompass_node_runtime::froglet_protocol;
use froglet_protocol::protocol::{
    ARTIFACT_TYPE_DEAL, ARTIFACT_TYPE_DESCRIPTOR, ARTIFACT_TYPE_OFFER, ARTIFACT_TYPE_QUOTE,
    ARTIFACT_TYPE_RECEIPT, DealPayload, QuotePayload, ReceiptPayload, SignedArtifact,
};
use froglet_verify::{DocumentStatus, validate_chain_documents};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Canonical schema identifier for Milestone C provenance bundles (`W6`).
pub const PROVENANCE_BUNDLE_SCHEMA_V1: &str = "bc.provenance.v1";

/// One artifact entry inside a run's `manifest_preimage` (matches `result_manifest_hash`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestPreimageArtifact {
    pub name: String,
    pub sha256: String,
    pub size_bytes: u64,
}

/// Preimage whose RFC 8785 JCS SHA-256 digest equals `receipt.payload.result_hash` (`W6`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestPreimage {
    pub result_json_sha256: String,
    #[serde(default)]
    pub artifacts: Vec<ManifestPreimageArtifact>,
}

/// One run included inside a `bc.provenance.v1` bundle (strictly ZERO `params`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceRunEntry {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_preimage: Option<ManifestPreimage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<RunInput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_receipt_hashes: Vec<String>,
    pub froglet_chain: Vec<Value>,
}

/// Exported `bc.provenance.v1` bundle envelope (`W6`, aligned with `compass-hub`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceBundle {
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exported_at: Option<String>,
    pub root_run_id: String,
    pub runs: Vec<ProvenanceRunEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_ancestors: Vec<Value>,
}

/// Cryptographically verified causal edge derived directly from signed `extension_refs`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct VerifiedProvenanceEdge {
    pub kind: String,
    pub from_run_id: String,
    pub to_run_id: String,
    pub ref_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_sha256: Option<String>,
}

/// Signed reference to an ancestor or child run that is absent from this bundle (`W6`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct UnresolvedAncestor {
    pub kind: String,
    pub referenced_by_run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_receipt_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_deal_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_receipt_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_sha256: Option<String>,
}

/// Per-run verification status inside [`ProvenanceVerificationReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedRunSummary {
    pub run_id: String,
    pub valid: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deal_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_hash: Option<String>,
    pub manifest_verified: bool,
    pub extension_refs_verified: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// Offline verification report printed by `biocompass verify-receipt` for `bc.provenance.v1` bundles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceVerificationReport {
    pub schema: String,
    pub valid: bool,
    pub complete: bool,
    pub acyclic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_run_id: Option<String>,
    pub runs: Vec<VerifiedRunSummary>,
    pub verified_edges: Vec<VerifiedProvenanceEdge>,
    pub unresolved_ancestors: Vec<UnresolvedAncestor>,
    pub errors: Vec<String>,
}

/// Returns `true` when `val` appears to be a `bc.provenance.*` bundle object rather than a
/// single-run `froglet/v1` chain document or array.
pub fn is_provenance_bundle_json(val: &Value) -> bool {
    val.as_object()
        .and_then(|obj| obj.get("schema"))
        .and_then(Value::as_str)
        .is_some_and(|s| s.starts_with("bc.provenance."))
}

struct ParsedVerifiedRun {
    run_id: String,
    parent_run_id: Option<String>,
    deal_hash: String,
    receipt_hash: String,
    parent_deal_hash: Option<String>,
    inputs: Vec<RunInput>,
    child_receipt_hashes: BTreeSet<String>,
    manifest_preimage: Option<ManifestPreimage>,
}

/// Offline-verify a `bc.provenance.v1` bundle without network or hub access (`W6`).
pub fn verify_provenance_bundle(val: &Value, now: Option<i64>) -> ProvenanceVerificationReport {
    let mut top_errors: Vec<String> = Vec::new();
    let schema = val
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    if schema != PROVENANCE_BUNDLE_SCHEMA_V1 {
        top_errors.push(format!(
            "unsupported provenance bundle schema '{schema}'; expected '{PROVENANCE_BUNDLE_SCHEMA_V1}'"
        ));
        return ProvenanceVerificationReport {
            schema,
            valid: false,
            complete: false,
            acyclic: true,
            root_run_id: None,
            runs: Vec::new(),
            verified_edges: Vec::new(),
            unresolved_ancestors: Vec::new(),
            errors: top_errors,
        };
    }

    // Privacy invariant: `bc.provenance.v1` MUST NOT include `params` on any run entry.
    if let Some(raw_runs) = val.get("runs").and_then(Value::as_array) {
        for (idx, raw_run) in raw_runs.iter().enumerate() {
            if raw_run.as_object().is_some_and(|obj| obj.contains_key("params")) {
                let label = raw_run
                    .get("run_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("runs[{idx}]"));
                top_errors.push(format!(
                    "privacy violation: bc.provenance.v1 run '{label}' must not contain 'params'"
                ));
            }
        }
    }

    let bundle: ProvenanceBundle = match serde_json::from_value(val.clone()) {
        Ok(b) => b,
        Err(e) => {
            top_errors.push(format!("invalid bc.provenance.v1 bundle structure: {e}"));
            return ProvenanceVerificationReport {
                schema,
                valid: false,
                complete: false,
                acyclic: true,
                root_run_id: None,
                runs: Vec::new(),
                verified_edges: Vec::new(),
                unresolved_ancestors: Vec::new(),
                errors: top_errors,
            };
        }
    };

    if bundle.runs.is_empty() {
        top_errors.push("provenance bundle contains zero runs".into());
    }

    let mut seen_run_ids = HashSet::new();
    let mut receipt_to_run: HashMap<String, String> = HashMap::new();
    let mut deal_to_run: HashMap<String, String> = HashMap::new();
    let mut parsed_runs: BTreeMap<String, ParsedVerifiedRun> = BTreeMap::new();
    let mut summaries: Vec<VerifiedRunSummary> = Vec::with_capacity(bundle.runs.len());

    let expected_kinds = [
        ARTIFACT_TYPE_DESCRIPTOR,
        ARTIFACT_TYPE_OFFER,
        ARTIFACT_TYPE_QUOTE,
        ARTIFACT_TYPE_DEAL,
        ARTIFACT_TYPE_RECEIPT,
    ];

    for entry in &bundle.runs {
        let mut run_errors = Vec::new();
        if entry.run_id.trim().is_empty() {
            run_errors.push("run entry has empty run_id".into());
        } else if !seen_run_ids.insert(entry.run_id.clone()) {
            let msg = format!("duplicate run_id '{}' in provenance bundle", entry.run_id);
            run_errors.push(msg.clone());
            top_errors.push(msg);
        }

        // 1. Verify all 5 Froglet chain documents + signatures + inter-document hashes.
        let chain_report = validate_chain_documents(&entry.froglet_chain, now);
        let chain_ok = chain_report.valid
            && entry.froglet_chain.len() == expected_kinds.len()
            && chain_report.artifacts.len() == expected_kinds.len()
            && chain_report
                .artifacts
                .iter()
                .zip(expected_kinds)
                .all(|(art, kind)| {
                    art.status == DocumentStatus::Verified && art.artifact_type == kind
                });
        if !chain_ok {
            let detail = if !chain_report.structure_errors.is_empty() {
                chain_report.structure_errors.join("; ")
            } else {
                serde_json::to_string(&chain_report).unwrap_or_else(|_| {
                    format!(
                        "expected 5 verified Froglet chain docs [Descriptor, Offer, Quote, Deal, Receipt], found {}",
                        entry.froglet_chain.len()
                    )
                })
            };
            run_errors.push(format!("froglet chain verification failed: {detail}"));
        }

        let decoded_quote: Option<SignedArtifact<QuotePayload>> = entry
            .froglet_chain
            .get(2)
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let decoded_deal: Option<SignedArtifact<DealPayload>> = entry
            .froglet_chain
            .get(3)
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let decoded_receipt: Option<SignedArtifact<ReceiptPayload>> = entry
            .froglet_chain
            .get(4)
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        let (Some(quote), Some(deal), Some(receipt)) =
            (decoded_quote, decoded_deal, decoded_receipt)
        else {
            if run_errors.is_empty() {
                run_errors.push("failed to decode Quote, Deal, or Receipt payload".into());
            }
            for e in &run_errors {
                top_errors.push(format!("run '{}': {e}", entry.run_id));
            }
            summaries.push(VerifiedRunSummary {
                run_id: entry.run_id.clone(),
                valid: false,
                deal_hash: None,
                receipt_hash: None,
                result_hash: None,
                manifest_verified: false,
                extension_refs_verified: false,
                errors: run_errors,
            });
            continue;
        };

        // 2. Verify manifest_preimage hash commitment against signed receipt.payload.result_hash.
        let mut manifest_verified = false;
        match (&receipt.payload.result_hash, &entry.manifest_preimage) {
            (Some(expected_result_hash), Some(preimage)) => {
                match verify_manifest_preimage(preimage, expected_result_hash) {
                    Ok(()) => manifest_verified = true,
                    Err(msg) => run_errors.push(msg),
                }
            }
            (Some(expected_result_hash), None) => {
                run_errors.push(format!(
                    "missing manifest_preimage for signed result_hash '{expected_result_hash}'"
                ));
            }
            (None, Some(_)) => {
                run_errors.push(
                    "unexpected manifest_preimage on run without signed receipt result_hash".into(),
                );
            }
            (None, None) => {
                manifest_verified = true;
            }
        }

        // 3. Verify Quote, Deal, and Receipt extension_refs via core_traits::commitments.
        let mut extension_refs_verified = true;
        let mut observed_parent_hashes = Vec::new();
        for raw_ref in &quote.payload.extension_refs {
            match parse_extension_ref(raw_ref) {
                Ok(ParsedExtensionRef::Input { .. }) => {}
                Ok(ParsedExtensionRef::Parent { parent_deal_hash }) => {
                    observed_parent_hashes.push(parent_deal_hash);
                }
                Ok(ParsedExtensionRef::Child { .. }) => {
                    extension_refs_verified = false;
                    run_errors.push(format!(
                        "bc.child.v1 extension_ref '{raw_ref}' is not allowed on Quote"
                    ));
                }
                Err(e) => {
                    extension_refs_verified = false;
                    run_errors.push(format!("invalid Quote extension_ref '{raw_ref}': {e}"));
                }
            }
        }
        if observed_parent_hashes.len() > 1 {
            extension_refs_verified = false;
            run_errors.push("Quote contains multiple bc.parent.v1 extension_refs".into());
        }
        let parent_deal_hash = observed_parent_hashes.into_iter().next();
        if entry.parent_run_id.is_some() && parent_deal_hash.is_none() {
            extension_refs_verified = false;
            run_errors.push(
                "run declares parent_run_id but signed Quote is missing bc.parent.v1 extension_ref"
                    .into(),
            );
        }
        if entry.parent_run_id.is_none() && parent_deal_hash.is_some() {
            extension_refs_verified = false;
            run_errors.push(
                "signed Quote carries bc.parent.v1 extension_ref but run entry omits parent_run_id"
                    .into(),
            );
        }

        if let Err(e) = verify_extension_refs(
            ExtensionRefStage::QuoteOrDeal,
            &quote.payload.extension_refs,
            &entry.inputs,
            parent_deal_hash.as_deref(),
            &[],
        ) {
            extension_refs_verified = false;
            run_errors.push(format!("Quote extension_refs mismatch: {e}"));
        }
        if let Err(e) = verify_extension_refs(
            ExtensionRefStage::QuoteOrDeal,
            &deal.payload.extension_refs,
            &entry.inputs,
            parent_deal_hash.as_deref(),
            &[],
        ) {
            extension_refs_verified = false;
            run_errors.push(format!("Deal extension_refs mismatch: {e}"));
        }
        if let Err(e) = verify_extension_refs(
            ExtensionRefStage::Receipt,
            &receipt.payload.extension_refs,
            &[],
            None,
            &entry.child_receipt_hashes,
        ) {
            extension_refs_verified = false;
            run_errors.push(format!("Receipt extension_refs mismatch: {e}"));
        }

        if let Some(prev) = deal_to_run.insert(deal.hash.clone(), entry.run_id.clone())
            && prev != entry.run_id
        {
            run_errors.push(format!(
                "duplicate deal hash '{}' shared with run '{prev}'",
                deal.hash
            ));
        }
        if let Some(prev) = receipt_to_run.insert(receipt.hash.clone(), entry.run_id.clone())
            && prev != entry.run_id
        {
            run_errors.push(format!(
                "duplicate receipt hash '{}' shared with run '{prev}'",
                receipt.hash
            ));
        }

        let run_valid = run_errors.is_empty();
        for e in &run_errors {
            top_errors.push(format!("run '{}': {e}", entry.run_id));
        }
        summaries.push(VerifiedRunSummary {
            run_id: entry.run_id.clone(),
            valid: run_valid,
            deal_hash: Some(deal.hash.clone()),
            receipt_hash: Some(receipt.hash.clone()),
            result_hash: receipt.payload.result_hash.clone(),
            manifest_verified,
            extension_refs_verified,
            errors: run_errors,
        });

        parsed_runs.insert(
            entry.run_id.clone(),
            ParsedVerifiedRun {
                run_id: entry.run_id.clone(),
                parent_run_id: entry.parent_run_id.clone(),
                deal_hash: deal.hash,
                receipt_hash: receipt.hash,
                parent_deal_hash,
                inputs: entry.inputs.clone(),
                child_receipt_hashes: entry.child_receipt_hashes.iter().cloned().collect(),
                manifest_preimage: entry.manifest_preimage.clone(),
            },
        );
    }

    if !bundle.root_run_id.trim().is_empty() && !seen_run_ids.contains(&bundle.root_run_id) {
        top_errors.push(format!(
            "root_run_id '{}' is not present in bundle.runs",
            bundle.root_run_id
        ));
    }

    // 4. Cross-run causal link verification (inputs + bidirectional parent/child + unresolved ancestors).
    let mut verified_edges_set: BTreeSet<VerifiedProvenanceEdge> = BTreeSet::new();
    let mut unresolved_set: BTreeSet<UnresolvedAncestor> = BTreeSet::new();
    let mut adjacency: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for run_id in parsed_runs.keys() {
        adjacency.entry(run_id.clone()).or_default();
    }

    for run in parsed_runs.values() {
        // 4a. Input links (`bc.input.v1:<sha256>:<source_receipt_hash>`)
        for input in &run.inputs {
            if input.source_run_id == run.run_id {
                top_errors.push(format!(
                    "cycle violation: run '{}' references itself as an input source",
                    run.run_id
                ));
                adjacency
                    .entry(run.run_id.clone())
                    .or_default()
                    .insert(run.run_id.clone());
                continue;
            }
            if let Some(source_run_id) = receipt_to_run.get(&input.source_receipt_hash) {
                if source_run_id != &input.source_run_id {
                    top_errors.push(format!(
                        "run '{}' input '{}' claims source_run_id '{}', but source_receipt_hash '{}' belongs to '{}'",
                        run.run_id,
                        input.alias,
                        input.source_run_id,
                        input.source_receipt_hash,
                        source_run_id
                    ));
                    continue;
                }
                let source = &parsed_runs[source_run_id];
                let artifact_matches = source.manifest_preimage.as_ref().is_some_and(|preimage| {
                    preimage.artifacts.iter().any(|art| {
                        art.name == input.name
                            && art.sha256 == input.sha256
                            && art.size_bytes == input.size_bytes
                    })
                });
                if !artifact_matches {
                    top_errors.push(format!(
                        "run '{}' input '{}' artifact '{}' (sha256={}, size={}) does not match upstream run '{}' verified manifest_preimage",
                        run.run_id,
                        input.alias,
                        input.name,
                        input.sha256,
                        input.size_bytes,
                        source.run_id
                    ));
                    continue;
                }
                verified_edges_set.insert(VerifiedProvenanceEdge {
                    kind: "input".into(),
                    from_run_id: source.run_id.clone(),
                    to_run_id: run.run_id.clone(),
                    ref_hash: input.source_receipt_hash.clone(),
                    artifact_name: Some(input.name.clone()),
                    artifact_sha256: Some(input.sha256.clone()),
                });
                adjacency
                    .entry(source.run_id.clone())
                    .or_default()
                    .insert(run.run_id.clone());
            } else if let Some(existing) = parsed_runs.get(&input.source_run_id) {
                top_errors.push(format!(
                    "run '{}' input '{}' claims source_receipt_hash '{}', but bundle run '{}' has receipt_hash '{}'",
                    run.run_id,
                    input.alias,
                    input.source_receipt_hash,
                    existing.run_id,
                    existing.receipt_hash
                ));
            } else {
                unresolved_set.insert(UnresolvedAncestor {
                    kind: "input".into(),
                    referenced_by_run_id: run.run_id.clone(),
                    source_run_id: Some(input.source_run_id.clone()),
                    parent_run_id: None,
                    source_receipt_hash: Some(input.source_receipt_hash.clone()),
                    parent_deal_hash: None,
                    child_receipt_hash: None,
                    artifact_name: Some(input.name.clone()),
                    artifact_sha256: Some(input.sha256.clone()),
                });
            }
        }

        // 4b. Child -> Parent link (`bc.parent.v1:<parent_deal_hash>`)
        if let Some(parent_deal_hash) = &run.parent_deal_hash {
            if run.parent_run_id.as_deref() == Some(run.run_id.as_str()) {
                top_errors.push(format!(
                    "cycle violation: run '{}' references itself as parent_run_id",
                    run.run_id
                ));
                adjacency
                    .entry(run.run_id.clone())
                    .or_default()
                    .insert(run.run_id.clone());
            } else if let Some(parent_run_id) = deal_to_run.get(parent_deal_hash) {
                if run.parent_run_id.as_deref() != Some(parent_run_id.as_str()) {
                    top_errors.push(format!(
                        "child run '{}' claims parent_run_id {:?}, but bc.parent.v1 deal hash '{}' belongs to '{}'",
                        run.run_id, run.parent_run_id, parent_deal_hash, parent_run_id
                    ));
                } else {
                    let parent = &parsed_runs[parent_run_id];
                    if !parent.child_receipt_hashes.contains(&run.receipt_hash) {
                        top_errors.push(format!(
                            "parent run '{}' receipt does not commit to child run '{}' receipt '{}' via bc.child.v1",
                            parent.run_id, run.run_id, run.receipt_hash
                        ));
                    } else {
                        verified_edges_set.insert(VerifiedProvenanceEdge {
                            kind: "parent_child".into(),
                            from_run_id: parent.run_id.clone(),
                            to_run_id: run.run_id.clone(),
                            ref_hash: parent_deal_hash.clone(),
                            artifact_name: None,
                            artifact_sha256: None,
                        });
                        adjacency
                            .entry(parent.run_id.clone())
                            .or_default()
                            .insert(run.run_id.clone());
                    }
                }
            } else if let Some(claimed_parent_id) = &run.parent_run_id
                && let Some(existing_parent) = parsed_runs.get(claimed_parent_id)
            {
                top_errors.push(format!(
                    "child run '{}' claims bc.parent.v1 deal hash '{}', but bundle run '{}' has deal_hash '{}'",
                    run.run_id, parent_deal_hash, existing_parent.run_id, existing_parent.deal_hash
                ));
            } else {
                unresolved_set.insert(UnresolvedAncestor {
                    kind: "parent".into(),
                    referenced_by_run_id: run.run_id.clone(),
                    source_run_id: None,
                    parent_run_id: run.parent_run_id.clone(),
                    source_receipt_hash: None,
                    parent_deal_hash: Some(parent_deal_hash.clone()),
                    child_receipt_hash: None,
                    artifact_name: None,
                    artifact_sha256: None,
                });
            }
        }

        // 4c. Parent -> Child links (`bc.child.v1:<child_receipt_hash>`)
        for child_receipt_hash in &run.child_receipt_hashes {
            if let Some(child_run_id) = receipt_to_run.get(child_receipt_hash) {
                let child = &parsed_runs[child_run_id];
                if child.parent_deal_hash.as_deref() != Some(run.deal_hash.as_str())
                    || child.parent_run_id.as_deref() != Some(run.run_id.as_str())
                {
                    top_errors.push(format!(
                        "parent run '{}' commits to child '{}' (receipt '{}'), but child does not commit back to parent deal '{}'",
                        run.run_id, child.run_id, child_receipt_hash, run.deal_hash
                    ));
                } else {
                    verified_edges_set.insert(VerifiedProvenanceEdge {
                        kind: "parent_child".into(),
                        from_run_id: run.run_id.clone(),
                        to_run_id: child.run_id.clone(),
                        ref_hash: run.deal_hash.clone(),
                        artifact_name: None,
                        artifact_sha256: None,
                    });
                    adjacency
                        .entry(run.run_id.clone())
                        .or_default()
                        .insert(child.run_id.clone());
                }
            } else {
                unresolved_set.insert(UnresolvedAncestor {
                    kind: "child".into(),
                    referenced_by_run_id: run.run_id.clone(),
                    source_run_id: None,
                    parent_run_id: None,
                    source_receipt_hash: None,
                    parent_deal_hash: None,
                    child_receipt_hash: Some(child_receipt_hash.clone()),
                    artifact_name: None,
                    artifact_sha256: None,
                });
            }
        }
    }

    // 5. Verify DAG acyclicity across all causal edges.
    let acyclic = match check_dag_acyclic(&adjacency) {
        Ok(()) => true,
        Err(cycle_msg) => {
            top_errors.push(cycle_msg);
            false
        }
    };

    let valid = top_errors.is_empty() && acyclic && summaries.iter().all(|s| s.valid);
    let unresolved_ancestors: Vec<UnresolvedAncestor> = unresolved_set.into_iter().collect();
    let complete = valid && unresolved_ancestors.is_empty();

    ProvenanceVerificationReport {
        schema,
        valid,
        complete,
        acyclic,
        root_run_id: Some(bundle.root_run_id),
        runs: summaries,
        verified_edges: verified_edges_set.into_iter().collect(),
        unresolved_ancestors,
        errors: top_errors,
    }
}

fn verify_manifest_preimage(
    preimage: &ManifestPreimage,
    expected_result_hash: &str,
) -> Result<(), String> {
    if !is_sha256_hex(&preimage.result_json_sha256) {
        return Err(format!(
            "manifest_preimage has invalid result_json_sha256 '{}'",
            preimage.result_json_sha256
        ));
    }
    let mut seen_names = HashSet::new();
    let mut artifact_refs = Vec::with_capacity(preimage.artifacts.len());
    for art in &preimage.artifacts {
        if art.name.is_empty()
            || art.name.starts_with('/')
            || art.name.contains(['\\', '\0'])
            || art.name.split('/').any(|p| matches!(p, "" | "." | ".."))
        {
            return Err(format!(
                "manifest_preimage contains invalid artifact name '{}'",
                art.name
            ));
        }
        if !seen_names.insert(&art.name) {
            return Err(format!(
                "manifest_preimage contains duplicate artifact name '{}'",
                art.name
            ));
        }
        if !is_sha256_hex(&art.sha256) {
            return Err(format!(
                "manifest_preimage artifact '{}' has invalid sha256 '{}'",
                art.name, art.sha256
            ));
        }
        artifact_refs.push(ArtifactRef {
            name: art.name.clone(),
            blob_key: String::new(),
            media_type: String::new(),
            size_bytes: art.size_bytes,
            sha256: art.sha256.clone(),
        });
    }
    let computed = result_manifest_hash(&preimage.result_json_sha256, &artifact_refs)
        .map_err(|e| format!("compute manifest_preimage hash: {e}"))?;
    if computed != expected_result_hash {
        return Err(format!(
            "manifest_preimage hash '{computed}' does not match signed receipt result_hash '{expected_result_hash}'"
        ));
    }
    Ok(())
}

/// Three-color DFS cycle detector returning a human-readable cycle description on back-edge.
pub fn check_dag_acyclic(adjacency: &BTreeMap<String, BTreeSet<String>>) -> Result<(), String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Color {
        White,
        Gray,
        Black,
    }
    fn dfs(
        node: &str,
        adj: &BTreeMap<String, BTreeSet<String>>,
        colors: &mut BTreeMap<String, Color>,
        stack: &mut Vec<String>,
    ) -> Result<(), String> {
        colors.insert(node.to_owned(), Color::Gray);
        stack.push(node.to_owned());
        if let Some(neighbors) = adj.get(node) {
            for next in neighbors {
                match colors.get(next).copied().unwrap_or(Color::White) {
                    Color::Gray => {
                        let start = stack.iter().position(|n| n == next).unwrap_or(0);
                        let mut cycle = stack[start..].to_vec();
                        cycle.push(next.clone());
                        return Err(format!(
                            "provenance graph contains a cycle: {}",
                            cycle.join(" -> ")
                        ));
                    }
                    Color::White => dfs(next, adj, colors, stack)?,
                    Color::Black => {}
                }
            }
        }
        stack.pop();
        colors.insert(node.to_owned(), Color::Black);
        Ok(())
    }

    let mut colors = BTreeMap::new();
    let mut stack = Vec::new();
    for node in adjacency.keys() {
        if colors.get(node).copied().unwrap_or(Color::White) == Color::White {
            dfs(node, adjacency, &mut colors, &mut stack)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use biocompass_core_traits::expected_extension_refs;
    use froglet_protocol::ExecutionRuntime;
    use froglet_protocol::crypto::{
        NodeSigningKey, generate_signing_key, public_key_hex, sha256_hex, sign_message_hex,
    };
    use froglet_protocol::protocol::{
        DescriptorCapabilities, DescriptorPayload, ExecutionLimits, FROGLET_SCHEMA_V1,
        OfferExecutionProfile, OfferPayload, OfferPriceSchedule, PAYMENT_METHOD_FREE,
        QuoteSettlementTerms, ReceiptExecutor, ReceiptLegState, ReceiptSettlementLeg,
        ReceiptSettlementRefs, SETTLEMENT_METHOD_NONE, sign_artifact,
    };
    use serde_json::json;

    struct SignedRunFixture {
        run_id: String,
        parent_run_id: Option<String>,
        deal_hash: String,
        receipt_hash: String,
        manifest_preimage: ManifestPreimage,
        inputs: Vec<RunInput>,
        child_receipt_hashes: Vec<String>,
        froglet_chain: Vec<Value>,
    }

    impl SignedRunFixture {
        fn to_entry(&self) -> ProvenanceRunEntry {
            ProvenanceRunEntry {
                run_id: self.run_id.clone(),
                plugin_id: Some("alithea/demo".into()),
                plugin_version: Some("1.0.0".into()),
                status: Some("succeeded".into()),
                parent_run_id: self.parent_run_id.clone(),
                created_at: Some("2026-11-23T00:00:00Z".into()),
                completed_at: Some("2026-11-23T00:01:00Z".into()),
                manifest_preimage: Some(self.manifest_preimage.clone()),
                inputs: self.inputs.clone(),
                child_receipt_hashes: self.child_receipt_hashes.clone(),
                froglet_chain: self.froglet_chain.clone(),
            }
        }
    }

    fn sign_doc<T: Serialize + Clone>(
        key: &NodeSigningKey,
        artifact_type: &str,
        created_at: i64,
        payload: T,
    ) -> SignedArtifact<T> {
        let signer = public_key_hex(key);
        sign_artifact(
            &signer,
            |msg| sign_message_hex(key, msg),
            artifact_type,
            created_at,
            payload,
        )
        .expect("sign artifact")
    }

    #[allow(clippy::too_many_arguments)]
    fn build_signed_run_prefix(
        provider_key: &NodeSigningKey,
        requester_key: &NodeSigningKey,
        run_id: &str,
        now: i64,
        inputs: &[RunInput],
        parent_deal_hash: Option<&str>,
    ) -> (
        SignedArtifact<DescriptorPayload>,
        SignedArtifact<OfferPayload>,
        SignedArtifact<QuotePayload>,
        SignedArtifact<DealPayload>,
    ) {
        let provider_id = public_key_hex(provider_key);
        let requester_id = public_key_hex(requester_key);
        let limits = ExecutionLimits {
            max_input_bytes: 1024 * 1024,
            max_runtime_ms: 60_000,
            max_memory_bytes: 256 * 1024 * 1024,
            max_output_bytes: 1024 * 1024,
            fuel_limit: 0,
        };
        let descriptor = sign_doc(
            provider_key,
            ARTIFACT_TYPE_DESCRIPTOR,
            now,
            DescriptorPayload {
                provider_id: provider_id.clone(),
                descriptor_seq: 1,
                protocol_version: FROGLET_SCHEMA_V1.into(),
                expires_at: Some(now + 86_400),
                linked_identities: Vec::new(),
                transport_endpoints: Vec::new(),
                capabilities: DescriptorCapabilities {
                    service_kinds: vec!["compute.container.v1".into()],
                    execution_runtimes: vec![ExecutionRuntime::Container.as_str().into()],
                    max_concurrent_deals: Some(4),
                },
                accepted_payment_methods: vec![PAYMENT_METHOD_FREE.into()],
            },
        );
        let offer = sign_doc(
            provider_key,
            ARTIFACT_TYPE_OFFER,
            now,
            OfferPayload {
                provider_id: provider_id.clone(),
                offer_id: "alithea/demo#default".into(),
                descriptor_hash: descriptor.hash.clone(),
                expires_at: Some(now + 3600),
                offer_kind: "compute.container.v1".into(),
                settlement_method: SETTLEMENT_METHOD_NONE.into(),
                quote_ttl_secs: 600,
                execution_profile: OfferExecutionProfile {
                    runtime: ExecutionRuntime::Container,
                    package_kind: "oci_image".into(),
                    contract_version: "biocompass.oci.io.v1".into(),
                    access_handles: Vec::new(),
                    abi_version: "biocompass.oci.io.v1".into(),
                    capabilities: Vec::new(),
                    max_input_bytes: limits.max_input_bytes,
                    max_runtime_ms: limits.max_runtime_ms,
                    max_memory_bytes: limits.max_memory_bytes,
                    max_output_bytes: limits.max_output_bytes,
                    fuel_limit: 0,
                },
                price_schedule: OfferPriceSchedule {
                    base_fee_msat: 0,
                    success_fee_msat: 0,
                },
                terms_hash: None,
                confidential_profile_hash: None,
            },
        );
        let ext_refs = expected_extension_refs(
            ExtensionRefStage::QuoteOrDeal,
            inputs,
            parent_deal_hash,
            &[],
        )
        .expect("quote extension refs");
        let workload_hash = sha256_hex(format!("workload:{run_id}:{now}").as_bytes());
        let quote = sign_doc(
            provider_key,
            ARTIFACT_TYPE_QUOTE,
            now,
            QuotePayload {
                provider_id: provider_id.clone(),
                requester_id: requester_id.clone(),
                descriptor_hash: descriptor.hash.clone(),
                offer_hash: offer.hash.clone(),
                expires_at: now + 600,
                workload_kind: "compute.container.v1".into(),
                workload_hash: workload_hash.clone(),
                confidential_session_hash: None,
                capabilities_granted: Vec::new(),
                extension_refs: ext_refs.clone(),
                quote_use: None,
                settlement_terms: QuoteSettlementTerms {
                    method: SETTLEMENT_METHOD_NONE.into(),
                    destination_identity: String::new(),
                    base_fee_msat: 0,
                    success_fee_msat: 0,
                    max_base_invoice_expiry_secs: 0,
                    max_success_hold_expiry_secs: 0,
                    min_final_cltv_expiry: 0,
                },
                execution_limits: limits,
            },
        );
        let deal = sign_doc(
            requester_key,
            ARTIFACT_TYPE_DEAL,
            now,
            DealPayload {
                requester_id,
                provider_id,
                quote_hash: quote.hash.clone(),
                workload_hash,
                confidential_session_hash: None,
                extension_refs: ext_refs,
                authority_ref: None,
                supersedes_deal_hash: None,
                client_nonce: Some(format!("nonce-{run_id}")),
                success_payment_hash: "00".repeat(32),
                admission_deadline: now + 60,
                completion_deadline: now + 300,
                acceptance_deadline: now + 600,
            },
        );
        (descriptor, offer, quote, deal)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_signed_run(
        provider_key: &NodeSigningKey,
        run_id: &str,
        now: i64,
        parent_run_id: Option<String>,
        inputs: Vec<RunInput>,
        child_receipt_hashes: Vec<String>,
        artifacts: Vec<ManifestPreimageArtifact>,
        prefix: (
            SignedArtifact<DescriptorPayload>,
            SignedArtifact<OfferPayload>,
            SignedArtifact<QuotePayload>,
            SignedArtifact<DealPayload>,
        ),
    ) -> SignedRunFixture {
        let (descriptor, offer, quote, deal) = prefix;
        let result_json_sha256 = sha256_hex(format!("{{\"run\":\"{run_id}\"}}").as_bytes());
        let preimage = ManifestPreimage {
            result_json_sha256: result_json_sha256.clone(),
            artifacts: artifacts.clone(),
        };
        let art_refs: Vec<ArtifactRef> = artifacts
            .iter()
            .map(|a| ArtifactRef {
                name: a.name.clone(),
                blob_key: String::new(),
                media_type: String::new(),
                size_bytes: a.size_bytes,
                sha256: a.sha256.clone(),
            })
            .collect();
        let result_hash = result_manifest_hash(&result_json_sha256, &art_refs).unwrap();
        let receipt_ext_refs = expected_extension_refs(
            ExtensionRefStage::Receipt,
            &[],
            None,
            &child_receipt_hashes,
        )
        .unwrap();
        let empty_leg = ReceiptSettlementLeg {
            amount_msat: 0,
            invoice_hash: String::new(),
            payment_hash: String::new(),
            state: ReceiptLegState::Canceled,
        };
        let receipt = sign_doc(
            provider_key,
            ARTIFACT_TYPE_RECEIPT,
            now + 5,
            ReceiptPayload {
                provider_id: quote.payload.provider_id.clone(),
                requester_id: quote.payload.requester_id.clone(),
                deal_hash: deal.hash.clone(),
                quote_hash: quote.hash.clone(),
                extension_refs: receipt_ext_refs,
                acceptance_ref: None,
                started_at: Some(now),
                finished_at: now + 5,
                deal_state: "succeeded".into(),
                execution_state: "succeeded".into(),
                settlement_state: SETTLEMENT_METHOD_NONE.into(),
                result_hash: Some(result_hash),
                confidential_session_hash: None,
                result_envelope_hash: None,
                result_format: Some("application/json".into()),
                executor: ReceiptExecutor {
                    runtime: ExecutionRuntime::Container.as_str().into(),
                    runtime_version: "kubernetes:1.31".into(),
                    execution_mode: Some("airgapped".into()),
                    attestation_platform: None,
                    measurement: None,
                    abi_version: Some("biocompass.oci.io.v1".into()),
                    module_hash: Some("11".repeat(32)),
                    capabilities_granted: Vec::new(),
                },
                limits_applied: quote.payload.execution_limits.clone(),
                settlement_refs: ReceiptSettlementRefs {
                    method: SETTLEMENT_METHOD_NONE.into(),
                    bundle_hash: None,
                    destination_identity: String::new(),
                    base_fee: empty_leg.clone(),
                    success_fee: empty_leg,
                },
                failure_code: None,
                failure_message: None,
                result_ref: Some(format!("blob://runs/{run_id}/result.json")),
            },
        );
        let chain = vec![
            serde_json::to_value(&descriptor).unwrap(),
            serde_json::to_value(&offer).unwrap(),
            serde_json::to_value(&quote).unwrap(),
            serde_json::to_value(&deal).unwrap(),
            serde_json::to_value(&receipt).unwrap(),
        ];
        SignedRunFixture {
            run_id: run_id.into(),
            parent_run_id,
            deal_hash: deal.hash,
            receipt_hash: receipt.hash,
            manifest_preimage: preimage,
            inputs,
            child_receipt_hashes,
            froglet_chain: chain,
        }
    }

    #[test]
    fn golden_multihop_input_and_parent_child_bundle_verifies_completely() {
        let provider_key = generate_signing_key();
        let requester_key = generate_signing_key();
        let now = 1_760_000_000_i64;

        // Run A produces scores.parquet
        let art_a = ManifestPreimageArtifact {
            name: "scores.parquet".into(),
            sha256: sha256_hex(b"parquet-bytes-a"),
            size_bytes: 15,
        };
        let prefix_a = build_signed_run_prefix(&provider_key, &requester_key, "run-a", now, &[], None);
        let run_a = finish_signed_run(
            &provider_key,
            "run-a",
            now,
            None,
            Vec::new(),
            Vec::new(),
            vec![art_a.clone()],
            prefix_a,
        );

        // Run B consumes Run A's artifact
        let input_a = RunInput {
            alias: "upstream".into(),
            source_run_id: "run-a".into(),
            name: art_a.name.clone(),
            sha256: art_a.sha256.clone(),
            size_bytes: art_a.size_bytes,
            source_receipt_hash: run_a.receipt_hash.clone(),
        };
        let art_b = ManifestPreimageArtifact {
            name: "enriched.csv".into(),
            sha256: sha256_hex(b"csv-bytes-b"),
            size_bytes: 11,
        };
        let prefix_b = build_signed_run_prefix(
            &provider_key,
            &requester_key,
            "run-b",
            now + 10,
            std::slice::from_ref(&input_a),
            None,
        );
        let run_b = finish_signed_run(
            &provider_key,
            "run-b",
            now + 10,
            None,
            vec![input_a],
            Vec::new(),
            vec![art_b.clone()],
            prefix_b,
        );

        // Notebook parent P starts (Deal signed), spawns Child C consuming Run B's artifact, then P finishes committing to C's receipt!
        let prefix_p =
            build_signed_run_prefix(&provider_key, &requester_key, "run-parent", now + 20, &[], None);
        let parent_deal_hash = prefix_p.3.hash.clone();

        let input_b = RunInput {
            alias: "from-b".into(),
            source_run_id: "run-b".into(),
            name: art_b.name.clone(),
            sha256: art_b.sha256.clone(),
            size_bytes: art_b.size_bytes,
            source_receipt_hash: run_b.receipt_hash.clone(),
        };
        let prefix_c = build_signed_run_prefix(
            &provider_key,
            &requester_key,
            "run-child",
            now + 22,
            std::slice::from_ref(&input_b),
            Some(&parent_deal_hash),
        );
        let run_c = finish_signed_run(
            &provider_key,
            "run-child",
            now + 22,
            Some("run-parent".into()),
            vec![input_b],
            Vec::new(),
            Vec::new(),
            prefix_c,
        );
        let run_p = finish_signed_run(
            &provider_key,
            "run-parent",
            now + 25,
            None,
            Vec::new(),
            vec![run_c.receipt_hash.clone()],
            Vec::new(),
            prefix_p,
        );

        let bundle = ProvenanceBundle {
            schema: PROVENANCE_BUNDLE_SCHEMA_V1.into(),
            exported_at: Some("2026-11-23T00:02:00Z".into()),
            root_run_id: "run-parent".into(),
            runs: vec![
                run_a.to_entry(),
                run_b.to_entry(),
                run_c.to_entry(),
                run_p.to_entry(),
            ],
            edges: Vec::new(),
            unresolved_ancestors: Vec::new(),
        };
        let val = serde_json::to_value(&bundle).unwrap();
        assert!(is_provenance_bundle_json(&val));
        let report = verify_provenance_bundle(&val, None);
        assert!(report.valid, "expected valid report, errors={:?}", report.errors);
        assert!(report.complete, "expected complete bundle");
        assert!(report.acyclic);
        assert!(report.unresolved_ancestors.is_empty());
        assert_eq!(report.runs.len(), 4);
        assert!(report.runs.iter().any(|r| {
            r.run_id == "run-parent" && r.deal_hash.as_deref() == Some(run_p.deal_hash.as_str())
        }));
        assert_eq!(report.verified_edges.len(), 3);
        assert!(
            report
                .verified_edges
                .iter()
                .any(|e| e.kind == "input" && e.from_run_id == "run-a" && e.to_run_id == "run-b")
        );
        assert!(
            report
                .verified_edges
                .iter()
                .any(|e| e.kind == "input" && e.from_run_id == "run-b" && e.to_run_id == "run-child")
        );
        assert!(report.verified_edges.iter().any(|e| {
            e.kind == "parent_child" && e.from_run_id == "run-parent" && e.to_run_id == "run-child"
        }));
    }

    #[test]
    fn tamper_forged_input_digest_manifest_privacy_and_broken_parent_child_are_rejected() {
        let provider_key = generate_signing_key();
        let requester_key = generate_signing_key();
        let now = 1_760_000_000_i64;

        let art_a = ManifestPreimageArtifact {
            name: "scores.parquet".into(),
            sha256: sha256_hex(b"real-bytes"),
            size_bytes: 10,
        };
        let prefix_a = build_signed_run_prefix(&provider_key, &requester_key, "run-a", now, &[], None);
        let run_a = finish_signed_run(
            &provider_key,
            "run-a",
            now,
            None,
            Vec::new(),
            Vec::new(),
            vec![art_a.clone()],
            prefix_a,
        );

        // 1. Forged input digest: Run B is validly signed, but claims a DIFFERENT sha256 than Run A's signed manifest!
        let forged_input = RunInput {
            alias: "upstream".into(),
            source_run_id: "run-a".into(),
            name: art_a.name.clone(),
            sha256: sha256_hex(b"FORGED-BYTES"),
            size_bytes: 10,
            source_receipt_hash: run_a.receipt_hash.clone(),
        };
        let prefix_b_forged = build_signed_run_prefix(
            &provider_key,
            &requester_key,
            "run-b",
            now + 5,
            std::slice::from_ref(&forged_input),
            None,
        );
        let run_b_forged = finish_signed_run(
            &provider_key,
            "run-b",
            now + 5,
            None,
            vec![forged_input],
            Vec::new(),
            Vec::new(),
            prefix_b_forged,
        );
        let forged_bundle = serde_json::to_value(ProvenanceBundle {
            schema: PROVENANCE_BUNDLE_SCHEMA_V1.into(),
            exported_at: None,
            root_run_id: "run-b".into(),
            runs: vec![run_a.to_entry(), run_b_forged.to_entry()],
            edges: Vec::new(),
            unresolved_ancestors: Vec::new(),
        })
        .unwrap();
        let report = verify_provenance_bundle(&forged_bundle, None);
        assert!(!report.valid, "forged input digest must be rejected");
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.contains("manifest_preimage")),
            "expected manifest mismatch error, got {:?}",
            report.errors
        );

        // 2. Tampered manifest_preimage on Run A
        let mut tampered_manifest_entry = run_a.to_entry();
        tampered_manifest_entry
            .manifest_preimage
            .as_mut()
            .unwrap()
            .artifacts[0]
            .size_bytes = 999;
        let bad_manifest_bundle = serde_json::to_value(ProvenanceBundle {
            schema: PROVENANCE_BUNDLE_SCHEMA_V1.into(),
            exported_at: None,
            root_run_id: "run-a".into(),
            runs: vec![tampered_manifest_entry],
            edges: Vec::new(),
            unresolved_ancestors: Vec::new(),
        })
        .unwrap();
        let report = verify_provenance_bundle(&bad_manifest_bundle, None);
        assert!(!report.valid, "tampered manifest_preimage must be rejected");

        // 3. Privacy violation: leaking `params` inside `bc.provenance.v1`
        let mut leaked_bundle = serde_json::to_value(ProvenanceBundle {
            schema: PROVENANCE_BUNDLE_SCHEMA_V1.into(),
            exported_at: None,
            root_run_id: "run-a".into(),
            runs: vec![run_a.to_entry()],
            edges: Vec::new(),
            unresolved_ancestors: Vec::new(),
        })
        .unwrap();
        leaked_bundle["runs"][0]["params"] = json!({"secret_cohort": "patient-123"});
        let report = verify_provenance_bundle(&leaked_bundle, None);
        assert!(!report.valid, "leaked params in bc.provenance.v1 must fail");
        assert!(
            report.errors.iter().any(|e| e.contains("params")),
            "{:?}",
            report.errors
        );

        // 4. Broken parent/child binding: Child claims Parent's deal_hash, but Parent's Receipt omits bc.child.v1!
        let prefix_p =
            build_signed_run_prefix(&provider_key, &requester_key, "run-p", now + 20, &[], None);
        let parent_deal_hash = prefix_p.3.hash.clone();
        let prefix_c = build_signed_run_prefix(
            &provider_key,
            &requester_key,
            "run-c",
            now + 22,
            &[],
            Some(&parent_deal_hash),
        );
        let run_c = finish_signed_run(
            &provider_key,
            "run-c",
            now + 22,
            Some("run-p".into()),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            prefix_c,
        );
        // Parent finishes WITHOUT committing to run_c's receipt!
        let run_p_missing_child = finish_signed_run(
            &provider_key,
            "run-p",
            now + 25,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            prefix_p,
        );
        let broken_link_bundle = serde_json::to_value(ProvenanceBundle {
            schema: PROVENANCE_BUNDLE_SCHEMA_V1.into(),
            exported_at: None,
            root_run_id: "run-p".into(),
            runs: vec![run_p_missing_child.to_entry(), run_c.to_entry()],
            edges: Vec::new(),
            unresolved_ancestors: Vec::new(),
        })
        .unwrap();
        let report = verify_provenance_bundle(&broken_link_bundle, None);
        assert!(!report.valid, "missing bc.child.v1 on parent receipt must fail");
        assert!(
            report.errors.iter().any(|e| e.contains("bc.child.v1")),
            "{:?}",
            report.errors
        );

        // 5. Tampered Froglet signature on Run A receipt MUST be rejected!
        let mut bad_sig_bundle = serde_json::to_value(ProvenanceBundle {
            schema: PROVENANCE_BUNDLE_SCHEMA_V1.into(),
            exported_at: None,
            root_run_id: "run-a".into(),
            runs: vec![run_a.to_entry()],
            edges: Vec::new(),
            unresolved_ancestors: Vec::new(),
        })
        .unwrap();
        let orig_sig = bad_sig_bundle["runs"][0]["froglet_chain"][4]["signature"]
            .as_str()
            .unwrap()
            .to_owned();
        let flipped = if let Some(rest) = orig_sig.strip_prefix('0') {
            format!("1{rest}")
        } else {
            format!("0{}", &orig_sig[1..])
        };
        bad_sig_bundle["runs"][0]["froglet_chain"][4]["signature"] = json!(flipped);
        let report = verify_provenance_bundle(&bad_sig_bundle, None);
        assert!(!report.valid, "tampered Froglet signature must fail");
        assert!(
            report.errors.iter().any(|e| e.contains("froglet chain")),
            "{:?}",
            report.errors
        );
    }

    #[test]
    fn missing_ancestors_are_reported_clearly_as_unresolved_ancestors() {
        let provider_key = generate_signing_key();
        let requester_key = generate_signing_key();
        let now = 1_760_000_000_i64;

        let missing_receipt_hash = "ab".repeat(32);
        let missing_parent_deal = "cd".repeat(32);
        let missing_child_receipt = "ef".repeat(32);

        let missing_input = RunInput {
            alias: "external".into(),
            source_run_id: "run-missing-upstream".into(),
            name: "features.parquet".into(),
            sha256: "12".repeat(32),
            size_bytes: 512,
            source_receipt_hash: missing_receipt_hash.clone(),
        };
        let prefix = build_signed_run_prefix(
            &provider_key,
            &requester_key,
            "run-leaf",
            now,
            std::slice::from_ref(&missing_input),
            Some(&missing_parent_deal),
        );
        let run_leaf = finish_signed_run(
            &provider_key,
            "run-leaf",
            now,
            Some("run-missing-parent".into()),
            vec![missing_input],
            vec![missing_child_receipt.clone()],
            Vec::new(),
            prefix,
        );

        let val = serde_json::to_value(ProvenanceBundle {
            schema: PROVENANCE_BUNDLE_SCHEMA_V1.into(),
            exported_at: None,
            root_run_id: "run-leaf".into(),
            runs: vec![run_leaf.to_entry()],
            edges: Vec::new(),
            unresolved_ancestors: Vec::new(),
        })
        .unwrap();
        let report = verify_provenance_bundle(&val, None);
        assert!(report.valid, "valid partial bundle should verify cleanly: {:?}", report.errors);
        assert!(!report.complete, "missing ancestors must mark complete = false");
        assert!(report.acyclic);
        assert_eq!(report.unresolved_ancestors.len(), 3);
        assert!(report.unresolved_ancestors.iter().any(|u| {
            u.kind == "input"
                && u.source_run_id.as_deref() == Some("run-missing-upstream")
                && u.source_receipt_hash.as_deref() == Some(missing_receipt_hash.as_str())
        }));
        assert!(report.unresolved_ancestors.iter().any(|u| {
            u.kind == "parent"
                && u.parent_run_id.as_deref() == Some("run-missing-parent")
                && u.parent_deal_hash.as_deref() == Some(missing_parent_deal.as_str())
        }));
        assert!(report.unresolved_ancestors.iter().any(|u| {
            u.kind == "child"
                && u.child_receipt_hash.as_deref() == Some(missing_child_receipt.as_str())
        }));
    }

    #[test]
    fn cycle_tamper_is_detected_and_rejected() {
        // 1. Direct DAG cycle detector tests (2-cycle, 3-cycle, self-loop)
        let mut two_cycle = BTreeMap::new();
        two_cycle.insert("run-a".into(), BTreeSet::from(["run-b".into()]));
        two_cycle.insert("run-b".into(), BTreeSet::from(["run-a".into()]));
        let err = check_dag_acyclic(&two_cycle).expect_err("2-cycle must fail");
        assert!(err.contains("cycle"), "{err}");

        let mut three_cycle = BTreeMap::new();
        three_cycle.insert("run-1".into(), BTreeSet::from(["run-2".into()]));
        three_cycle.insert("run-2".into(), BTreeSet::from(["run-3".into()]));
        three_cycle.insert("run-3".into(), BTreeSet::from(["run-1".into()]));
        let err = check_dag_acyclic(&three_cycle).expect_err("3-cycle must fail");
        assert!(err.contains("cycle"), "{err}");

        // 2. End-to-end bundle tamper: self-referencing input/parent_run_id cycle
        let provider_key = generate_signing_key();
        let requester_key = generate_signing_key();
        let now = 1_760_000_000_i64;
        let self_input = RunInput {
            alias: "self-loop".into(),
            source_run_id: "run-self".into(),
            name: "out.csv".into(),
            sha256: "34".repeat(32),
            size_bytes: 10,
            source_receipt_hash: "56".repeat(32),
        };
        let prefix = build_signed_run_prefix(
            &provider_key,
            &requester_key,
            "run-self",
            now,
            std::slice::from_ref(&self_input),
            None,
        );
        let run_self = finish_signed_run(
            &provider_key,
            "run-self",
            now,
            None,
            vec![self_input],
            Vec::new(),
            Vec::new(),
            prefix,
        );
        let val = serde_json::to_value(ProvenanceBundle {
            schema: PROVENANCE_BUNDLE_SCHEMA_V1.into(),
            exported_at: None,
            root_run_id: "run-self".into(),
            runs: vec![run_self.to_entry()],
            edges: Vec::new(),
            unresolved_ancestors: Vec::new(),
        })
        .unwrap();
        let report = verify_provenance_bundle(&val, None);
        assert!(!report.valid, "self-loop cycle tamper must be rejected");
        assert!(!report.acyclic, "self-loop must mark acyclic = false");
        assert!(report.errors.iter().any(|e| e.contains("cycle")), "{:?}", report.errors);
    }
}
