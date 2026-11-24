//! Signed output, hosted/local workload, and provenance `extension_refs` commitments
//! shared by producers, verifiers, and pickup clients (`B6`, `W6`, `W7`).

use crate::{
    ArtifactRef, CompassError, CompassResult, DatasetGrant, PluginId, RunInput,
    canonicalize_run_inputs, is_sha256_hex,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Froglet `extension_refs` prefix for an upstream artifact input (`Quote` & `Deal`, `W6`).
pub const EXTENSION_REF_INPUT_PREFIX: &str = "bc.input.v1:";
/// Froglet `extension_refs` prefix for a parent run's Deal hash (`Quote` & `Deal` of a child run, `W7`).
pub const EXTENSION_REF_PARENT_PREFIX: &str = "bc.parent.v1:";
/// Froglet `extension_refs` prefix for a child run's Receipt hash (`Receipt` of a parent run, `W7`).
pub const EXTENSION_REF_CHILD_PREFIX: &str = "bc.child.v1:";

fn canonical_digest<T: Serialize>(document: &T) -> CompassResult<String> {
    let bytes = serde_json_canonicalizer::to_vec(document)
        .map_err(|error| CompassError::Protocol(format!("canonicalize commitment: {error}")))?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}

/// Froglet's result commitment for a BioCompass run uses the exact stored `result.json`
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

/// The single canonical BioCompass workload descriptor hashed via RFC 8785 JCS + SHA-256
/// to produce Froglet `workload_hash` across local `node-runtime`, `compass-hub`, and `froglet-node` (`B6`, `W6`).
///
/// `inputs` is omitted from canonical JSON whenever empty (`skip_serializing_if = "Vec::is_empty"`),
/// preserving byte-identical workload hashes for every pre-Milestone-C receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkloadDescriptor {
    pub plugin_id: PluginId,
    pub plugin_version: String,
    pub image_digest: String,
    pub params: Value,
    pub dataset_digest: Option<String>,
    pub grant: Option<DatasetGrant>,
    pub is_curated_approved: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<RunInput>,
}

impl WorkloadDescriptor {
    /// Validate and canonicalize `inputs`, then compute the deterministic RFC 8785 JCS SHA-256 digest.
    pub fn workload_hash(&self) -> CompassResult<String> {
        let canonical_inputs = canonicalize_run_inputs(&self.inputs)?;
        if canonical_inputs == self.inputs {
            canonical_digest(self)
        } else {
            let mut normalized = self.clone();
            normalized.inputs = canonical_inputs;
            canonical_digest(&normalized)
        }
    }
}

/// Compute the deterministic RFC 8785 JCS SHA-256 `workload_hash` over every execution-critical input (`B6`, `W6`).
///
/// `node-runtime`, `compass-hub`, and `froglet-node` all call this single function independently and
/// compare the result against the signed Froglet `Quote` & `Deal` before reserving EUR or launching any container.
#[allow(clippy::too_many_arguments)]
pub fn compute_canonical_workload_hash(
    plugin_id: &PluginId,
    plugin_version: &str,
    image_digest: &str,
    params: &Value,
    dataset_digest: Option<&str>,
    effective_grant: Option<&DatasetGrant>,
    is_curated_approved: bool,
    inputs: &[RunInput],
) -> CompassResult<String> {
    let canonical_inputs = canonicalize_run_inputs(inputs)?;
    let descriptor = WorkloadDescriptor {
        plugin_id: plugin_id.clone(),
        plugin_version: plugin_version.to_owned(),
        image_digest: image_digest.to_owned(),
        params: params.clone(),
        dataset_digest: dataset_digest.map(str::to_owned),
        grant: effective_grant.cloned(),
        is_curated_approved,
        inputs: canonical_inputs,
    };
    canonical_digest(&descriptor)
}

/// Froglet document stage governing which BioCompass provenance `extension_refs` are required (`W6`, `W7`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtensionRefStage {
    /// `Quote` and `Deal`: binds upstream inputs (`bc.input.v1:<sha256>:<source_receipt_hash>`)
    /// and optional parent deal link (`bc.parent.v1:<parent_deal_hash>`).
    QuoteOrDeal,
    /// `Receipt`: binds child run receipts spawned during execution (`bc.child.v1:<child_receipt_hash>`).
    Receipt,
}

/// Parsed BioCompass provenance extension reference from a signed Froglet artifact (`W6`, `W7`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ParsedExtensionRef {
    Input {
        sha256: String,
        source_receipt_hash: String,
    },
    Parent {
        parent_deal_hash: String,
    },
    Child {
        child_receipt_hash: String,
    },
}

/// Parse one BioCompass provenance `extension_refs` entry, rejecting malformed prefixes or non-64-hex digests.
pub fn parse_extension_ref(raw: &str) -> CompassResult<ParsedExtensionRef> {
    if let Some(rest) = raw.strip_prefix(EXTENSION_REF_INPUT_PREFIX) {
        let Some((sha256, source_receipt_hash)) = rest.split_once(':') else {
            return Err(CompassError::IntegrityViolation(format!(
                "malformed bc.input.v1 extension_ref '{raw}'"
            )));
        };
        if source_receipt_hash.contains(':')
            || !is_sha256_hex(sha256)
            || !is_sha256_hex(source_receipt_hash)
        {
            return Err(CompassError::IntegrityViolation(format!(
                "invalid SHA-256 digest in bc.input.v1 extension_ref '{raw}'"
            )));
        }
        return Ok(ParsedExtensionRef::Input {
            sha256: sha256.to_owned(),
            source_receipt_hash: source_receipt_hash.to_owned(),
        });
    }
    if let Some(parent_deal_hash) = raw.strip_prefix(EXTENSION_REF_PARENT_PREFIX) {
        if !is_sha256_hex(parent_deal_hash) {
            return Err(CompassError::IntegrityViolation(format!(
                "invalid SHA-256 digest in bc.parent.v1 extension_ref '{raw}'"
            )));
        }
        return Ok(ParsedExtensionRef::Parent {
            parent_deal_hash: parent_deal_hash.to_owned(),
        });
    }
    if let Some(child_receipt_hash) = raw.strip_prefix(EXTENSION_REF_CHILD_PREFIX) {
        if !is_sha256_hex(child_receipt_hash) {
            return Err(CompassError::IntegrityViolation(format!(
                "invalid SHA-256 digest in bc.child.v1 extension_ref '{raw}'"
            )));
        }
        return Ok(ParsedExtensionRef::Child {
            child_receipt_hash: child_receipt_hash.to_owned(),
        });
    }
    Err(CompassError::IntegrityViolation(format!(
        "unrecognized BioCompass extension_ref '{raw}'"
    )))
}

/// Compute the deterministic, deduplicated, lexicographically sorted `extension_refs` vector
/// for a Froglet `Quote`, `Deal`, or `Receipt` (`W6`, `W7`).
///
/// - [`ExtensionRefStage::QuoteOrDeal`]: emits `bc.input.v1:<sha256>:<source_receipt_hash>` for each
///   validated input plus `bc.parent.v1:<parent_deal_hash>` when `parent_deal_hash` is `Some`.
/// - [`ExtensionRefStage::Receipt`]: emits `bc.child.v1:<child_receipt_hash>` for each
///   validated 64-hex child receipt hash.
///
/// Returns an empty `Vec` when no provenance links apply, preserving Milestone A/B wire compatibility.
pub fn expected_extension_refs(
    stage: ExtensionRefStage,
    inputs: &[RunInput],
    parent_deal_hash: Option<&str>,
    child_receipt_hashes: &[String],
) -> CompassResult<Vec<String>> {
    let mut refs = BTreeSet::new();
    match stage {
        ExtensionRefStage::QuoteOrDeal => {
            let canonical_inputs = canonicalize_run_inputs(inputs)?;
            for input in canonical_inputs {
                refs.insert(format!(
                    "{EXTENSION_REF_INPUT_PREFIX}{}:{}",
                    input.sha256, input.source_receipt_hash
                ));
            }
            if let Some(parent_hash) = parent_deal_hash {
                if !is_sha256_hex(parent_hash) {
                    return Err(CompassError::Validation(format!(
                        "invalid parent_deal_hash '{parent_hash}': expected 64 lowercase hex chars"
                    )));
                }
                refs.insert(format!("{EXTENSION_REF_PARENT_PREFIX}{parent_hash}"));
            }
        }
        ExtensionRefStage::Receipt => {
            for child_hash in child_receipt_hashes {
                if !is_sha256_hex(child_hash) {
                    return Err(CompassError::Validation(format!(
                        "invalid child_receipt_hash '{child_hash}': expected 64 lowercase hex chars"
                    )));
                }
                refs.insert(format!("{EXTENSION_REF_CHILD_PREFIX}{child_hash}"));
            }
        }
    }
    Ok(refs.into_iter().collect())
}

/// Fail-closed verification that signed Froglet `extension_refs` match [`expected_extension_refs`].
pub fn verify_extension_refs(
    stage: ExtensionRefStage,
    actual: &[String],
    inputs: &[RunInput],
    parent_deal_hash: Option<&str>,
    child_receipt_hashes: &[String],
) -> CompassResult<()> {
    let expected = expected_extension_refs(stage, inputs, parent_deal_hash, child_receipt_hashes)?;
    if actual != expected {
        return Err(CompassError::IntegrityViolation(format!(
            "signed extension_refs mismatch for {stage:?}: expected {expected:?}, got {actual:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CatalogId, GrantLevel, OrgId};

    /// Golden vector proving input-less `WorkloadDescriptor` hashes are byte-identical
    /// to pre-Milestone-C signed receipts (`node-runtime/tests/fixtures/released_run.json`).
    #[test]
    fn golden_vector_inputless_workload_hash_matches_milestone_ab_receipt() {
        let grant = DatasetGrant {
            grant_id: "g".into(),
            label: "test".into(),
            org_id: OrgId::from("org-a"),
            catalog_id: CatalogId::from("fixture/table"),
            level: GrantLevel::Rows,
            row_scope: None,
            daily_row_budget: None,
            max_rows_per_query: 10,
            allow_local_export: false,
        };
        let image_digest = "11".repeat(32);
        let dataset_digest = "22".repeat(32);
        let params = json!({ "cohort_filter": { "snapshot_id": 2 } });

        let hash = compute_canonical_workload_hash(
            &PluginId::from("fixture/echo"),
            "1.0",
            &image_digest,
            &params,
            Some(&dataset_digest),
            Some(&grant),
            true,
            &[],
        )
        .expect("compute golden hash");

        // Exact workload_hash committed inside `crates/node-runtime/tests/fixtures/released_run.json`.
        assert_eq!(
            hash,
            "aa2a0af9b523468545cfc5f0851fc48969769216915c870b556a32bb1b83c8f7"
        );

        // Verify serialized WorkloadDescriptor omits `"inputs"` completely when empty
        // and produces the exact same JCS bytes as the pre-Milestone-C json!({...}) descriptor.
        let descriptor = WorkloadDescriptor {
            plugin_id: PluginId::from("fixture/echo"),
            plugin_version: "1.0".into(),
            image_digest: image_digest.clone(),
            params: params.clone(),
            dataset_digest: Some(dataset_digest.clone()),
            grant: Some(grant.clone()),
            is_curated_approved: true,
            inputs: Vec::new(),
        };
        let legacy_json_descriptor = json!({
            "plugin_id": PluginId::from("fixture/echo"),
            "plugin_version": "1.0",
            "image_digest": image_digest,
            "params": params,
            "dataset_digest": dataset_digest,
            "grant": grant,
            "is_curated_approved": true,
        });
        assert_eq!(
            serde_json_canonicalizer::to_vec(&descriptor).unwrap(),
            serde_json_canonicalizer::to_vec(&legacy_json_descriptor).unwrap(),
            "input-less WorkloadDescriptor must be byte-identical to Milestone A/B canonical JSON"
        );
        let encoded = serde_json::to_value(&descriptor).expect("serialize descriptor");
        assert!(
            encoded.get("inputs").is_none(),
            "empty inputs must be omitted from canonical workload JSON"
        );
        assert_eq!(descriptor.workload_hash().unwrap(), hash);
    }

    #[test]
    fn workload_hash_canonicalizes_inputs_order_and_binds_every_field() {
        let input_a = RunInput {
            alias: "alpha".into(),
            source_run_id: "run-1".into(),
            name: "scores.parquet".into(),
            sha256: "aa".repeat(32),
            size_bytes: 1024,
            source_receipt_hash: "bb".repeat(32),
        };
        let input_b = RunInput {
            alias: "beta".into(),
            source_run_id: "run-2".into(),
            name: "nested/motifs.csv".into(),
            sha256: "cc".repeat(32),
            size_bytes: 512,
            source_receipt_hash: "dd".repeat(32),
        };
        let img = "11".repeat(32);
        let params = json!({"k": 9});

        let h_empty = compute_canonical_workload_hash(
            &PluginId::from("alithea/demo"),
            "1.0.0",
            &img,
            &params,
            None,
            None,
            false,
            &[],
        )
        .unwrap();
        let h_ab = compute_canonical_workload_hash(
            &PluginId::from("alithea/demo"),
            "1.0.0",
            &img,
            &params,
            None,
            None,
            false,
            &[input_a.clone(), input_b.clone()],
        )
        .unwrap();
        let h_ba = compute_canonical_workload_hash(
            &PluginId::from("alithea/demo"),
            "1.0.0",
            &img,
            &params,
            None,
            None,
            false,
            &[input_b, input_a],
        )
        .unwrap();

        assert_ne!(h_empty, h_ab, "adding inputs must change workload_hash");
        assert_eq!(h_ab, h_ba, "input order must not affect workload_hash");
    }

    #[test]
    fn expected_and_parsed_extension_refs_cover_inputs_parent_and_children() {
        let input_1 = RunInput {
            alias: "upstream".into(),
            source_run_id: "run-a".into(),
            name: "out.parquet".into(),
            sha256: "11".repeat(32),
            size_bytes: 42,
            source_receipt_hash: "22".repeat(32),
        };
        let input_2 = RunInput {
            alias: "second".into(),
            source_run_id: "run-b".into(),
            name: "table.csv".into(),
            sha256: "00".repeat(32),
            size_bytes: 10,
            source_receipt_hash: "33".repeat(32),
        };
        let parent_deal = "44".repeat(32);
        let child_1 = "66".repeat(32);
        let child_2 = "55".repeat(32);

        // Empty provenance produces empty vec (Milestone A/B compatible).
        assert!(
            expected_extension_refs(ExtensionRefStage::QuoteOrDeal, &[], None, &[])
                .unwrap()
                .is_empty()
        );
        assert!(
            expected_extension_refs(ExtensionRefStage::Receipt, &[], None, &[])
                .unwrap()
                .is_empty()
        );

        let qd = expected_extension_refs(
            ExtensionRefStage::QuoteOrDeal,
            &[input_1.clone(), input_2.clone()],
            Some(&parent_deal),
            std::slice::from_ref(&child_1), // ignored for QuoteOrDeal
        )
        .unwrap();
        assert_eq!(
            qd,
            vec![
                format!("bc.input.v1:{}:{}", "00".repeat(32), "33".repeat(32)),
                format!("bc.input.v1:{}:{}", "11".repeat(32), "22".repeat(32)),
                format!("bc.parent.v1:{parent_deal}"),
            ]
        );
        verify_extension_refs(
            ExtensionRefStage::QuoteOrDeal,
            &qd,
            &[input_2, input_1],
            Some(&parent_deal),
            &[],
        )
        .unwrap();
        assert!(
            verify_extension_refs(
                ExtensionRefStage::QuoteOrDeal,
                &[],
                &[],
                Some(&parent_deal),
                &[],
            )
            .is_err()
        );

        let rec = expected_extension_refs(
            ExtensionRefStage::Receipt,
            &[],
            Some(&parent_deal), // ignored for Receipt
            &[child_1.clone(), child_2.clone(), child_1.clone()], // deduplicated & sorted
        )
        .unwrap();
        assert_eq!(
            rec,
            vec![
                format!("bc.child.v1:{child_2}"),
                format!("bc.child.v1:{child_1}"),
            ]
        );
        for raw in qd.iter().chain(rec.iter()) {
            assert!(parse_extension_ref(raw).is_ok(), "parse {raw}");
        }
        assert!(parse_extension_ref("bc.input.v1:not-hex:also-bad").is_err());
        assert!(parse_extension_ref("bc.unknown.v1:abcd").is_err());
    }

    #[test]
    fn run_inputs_whose_stage_paths_collide_case_insensitively_are_rejected() {
        let mk = |alias: &str, name: &str| crate::RunInput {
            alias: alias.into(),
            source_run_id: "run-src".into(),
            name: name.into(),
            sha256: "ab".repeat(32),
            size_bytes: 1,
            source_receipt_hash: "cd".repeat(32),
        };
        let err = crate::canonicalize_run_inputs(&[mk("a", "x.csv"), mk("A", "X.csv")]).unwrap_err();
        assert!(err.to_string().contains("case-insensitively"), "{err}");
        assert!(crate::canonicalize_run_inputs(&[mk("a", "x.csv"), mk("a", "X.CSV")]).is_err());
        crate::canonicalize_run_inputs(&[mk("a", "x.csv"), mk("b", "x.csv")]).unwrap();
    }
}
