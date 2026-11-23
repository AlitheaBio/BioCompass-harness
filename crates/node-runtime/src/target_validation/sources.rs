//! Physical HLA inputs for the provider's staged exports and declared live reads.

use super::{
    CONTRACT, OFF_TARGET_TOXICITY_CONTRACT, TOXICITY_SCREENING_CONTRACT, has_columns,
    validate_scientific_schema,
};
use crate::hla_contract;
use biocompass_core_traits::{CompassError, CompassResult, PluginId};
use biocompass_data_engine::SealedSnapshot;
use serde_json::Value;
use std::collections::BTreeSet;

pub(super) struct SourceGroup(&'static [(&'static str, &'static [&'static str])]);

impl SourceGroup {
    pub(super) fn available(&self, snapshot: &SealedSnapshot) -> bool {
        self.0
            .iter()
            .all(|(name, columns)| has_columns(snapshot, name, columns))
    }

    fn add_to(&self, selected: &mut BTreeSet<String>) {
        selected.extend(self.0.iter().map(|(name, _)| (*name).to_owned()));
    }
}

pub(super) const TYPING: SourceGroup = SourceGroup(&[(
    "obs_sample_hla_typing",
    &["original_sample_number", "allele_name"],
)]);
pub(super) const PROTEIN_ANNOTATION: SourceGroup = SourceGroup(&[
    ("reference_proteins", &["accession"]),
    ("vocab_protein_peptide_mappings", &["sequence", "accession"]),
]);
pub(super) const PREDICTIONS: SourceGroup = SourceGroup(&[(
    "vocab_peptide_binding_predictions",
    &["sequence", "allele_name"],
)]);
const BINDER_SCORING: SourceGroup = SourceGroup(&[
    (
        "vocab_peptide_binding_predictions",
        &["tool", "tool_version"],
    ),
    (
        "obs_sample_allele_scoring",
        &[
            "original_sample_number",
            "allele_name",
            "scored_allele_name",
            "tool",
            "tool_version",
        ],
    ),
]);

pub(super) fn can_score_binders(snapshot: &SealedSnapshot) -> bool {
    PREDICTIONS.available(snapshot) && BINDER_SCORING.available(snapshot)
}

pub(super) fn can_rank_binders(snapshot: &SealedSnapshot) -> bool {
    can_score_binders(snapshot)
        && has_columns(
            snapshot,
            "vocab_peptide_binding_predictions",
            &["raw_score"],
        )
}

/// The roster is always read to enforce the grant. A count query also reads measurements.
pub(super) fn runtime_read_source(query: &str) -> Option<&'static str> {
    match query {
        "sample_roster.v1" | "sample_roster.v2" => Some("obs_samples"),
        "sample_observation_counts.v1" | "sample_observation_counts.v2" => {
            Some("obs_sample_peptide_mappings")
        }
        _ => None,
    }
}

/// Return the tables used by the reviewed module or its declared live queries on
/// this specific seal. Optional enrichments share their schema tests with SQL registration.
/// Grant filtering still takes place in the query session before any rows reach the module.
pub fn reviewed_hla_input_tables(
    plugin_id: &PluginId,
    snapshot: &SealedSnapshot,
    params: &Value,
    runtime_queries: &[String],
) -> CompassResult<BTreeSet<String>> {
    let contract = hla_contract(plugin_id)?;
    let mut selected = BTreeSet::from(["obs_samples".to_owned()]);
    if !runtime_queries.is_empty() {
        // Every named query constructs the scoped roster, including a sample-class grant.
        for id in runtime_queries {
            let source = runtime_read_source(id).ok_or_else(|| {
                CompassError::Validation(format!("Unknown runtime HLA query '{id}'."))
            })?;
            selected.insert(source.to_owned());
        }
        for table in &selected {
            if !has_columns(snapshot, table, &["original_sample_number"]) {
                return Err(CompassError::Validation(format!(
                    "Missing required live query input: {table}.original_sample_number"
                )));
            }
        }
        return Ok(selected);
    }
    validate_scientific_schema(contract, snapshot, params)?;
    selected.insert("obs_sample_peptide_mappings".to_owned());
    if PROTEIN_ANNOTATION.available(snapshot) {
        PROTEIN_ANNOTATION.add_to(&mut selected);
    }
    if matches!(contract, CONTRACT | TOXICITY_SCREENING_CONTRACT) && TYPING.available(snapshot) {
        TYPING.add_to(&mut selected);
    }
    let include_binders = if contract == CONTRACT {
        can_rank_binders(snapshot)
    } else {
        can_score_binders(snapshot)
    };
    if include_binders {
        BINDER_SCORING.add_to(&mut selected);
    }
    if contract == OFF_TARGET_TOXICITY_CONTRACT {
        selected.insert("vocab_peptides".to_owned());
        if PREDICTIONS.available(snapshot) {
            PREDICTIONS.add_to(&mut selected);
        }
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use biocompass_core_traits::{CatalogId, ColumnDescriptor, TableSchema};
    use biocompass_data_engine::SealedTableSnapshot;
    use serde_json::json;

    fn snapshot(tables: &[(&str, &[&str])]) -> SealedSnapshot {
        let tables: Vec<_> = tables
            .iter()
            .map(|(name, columns)| SealedTableSnapshot {
                schema: TableSchema {
                    name: (*name).into(),
                    columns: columns
                        .iter()
                        .map(|name| ColumnDescriptor {
                            name: (*name).into(),
                            data_type: "VARCHAR".into(),
                            nullable: false,
                        })
                        .collect(),
                    primary_key: Vec::new(),
                    row_count: 0,
                },
                files: Vec::new(),
            })
            .collect();
        let catalog_id = CatalogId::from("fixture");
        SealedSnapshot {
            dataset_digest: SealedSnapshot::compute_dataset_digest(&catalog_id, 1, &tables),
            catalog_id,
            snapshot_id: 1,
            tables,
            created_at: String::new(),
            commit_message: String::new(),
            facets: Default::default(),
        }
    }

    fn names(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn staged_dependencies_include_only_usable_enrichments() {
        let base = [
            (
                "obs_samples",
                &["original_sample_number", "sample_class", "annotation"][..],
            ),
            (
                "obs_sample_peptide_mappings",
                &["original_sample_number", "sequence", "normalized_intensity"][..],
            ),
            ("vocab_peptides", &["sequence"][..]),
            (
                "vocab_peptide_binding_predictions",
                &["sequence", "allele_name", "tool", "tool_version"][..],
            ),
            (
                "obs_sample_hla_typing",
                &["original_sample_number", "allele_name"][..],
            ),
            ("reference_proteins", &["accession"][..]),
            ("unrelated_raw_files", &["sample"][..]),
        ];
        let target = PluginId::from("alithea/bc-target-validation");
        let screening = PluginId::from("alithea/bc-toxicity-screening");
        let off_target = PluginId::from("alithea/bc-off-target-toxicity");
        let params = json!({"search_type":"peptide","search_term":"X"});
        let partial = snapshot(&base);
        let roster = names(&[
            "obs_samples",
            "obs_sample_peptide_mappings",
            "obs_sample_hla_typing",
        ]);
        assert_eq!(
            reviewed_hla_input_tables(&screening, &partial, &params, &[]).unwrap(),
            roster
        );
        assert_eq!(
            reviewed_hla_input_tables(&target, &partial, &params, &[]).unwrap(),
            roster
        );
        assert_eq!(
            reviewed_hla_input_tables(&off_target, &partial, &params, &[]).unwrap(),
            names(&[
                "obs_samples",
                "obs_sample_peptide_mappings",
                "vocab_peptides",
                "vocab_peptide_binding_predictions"
            ])
        );
        let mut complete = base.to_vec();
        complete.extend_from_slice(&[
            (
                "vocab_protein_peptide_mappings",
                &["sequence", "accession"][..],
            ),
            (
                "obs_sample_allele_scoring",
                &[
                    "original_sample_number",
                    "allele_name",
                    "scored_allele_name",
                    "tool",
                    "tool_version",
                ][..],
            ),
        ]);
        let full = snapshot(&complete);
        let targets = reviewed_hla_input_tables(&target, &full, &params, &[]).unwrap();
        assert!(
            targets.contains("reference_proteins")
                && targets.contains("vocab_protein_peptide_mappings")
        );
        assert!(!targets.contains("obs_sample_allele_scoring")); // Target's ranked binders need raw_score.
        let screening_tables = reviewed_hla_input_tables(&screening, &full, &params, &[]).unwrap();
        assert!(
            screening_tables.contains("obs_sample_allele_scoring")
                && screening_tables.contains("vocab_peptide_binding_predictions")
        );
        assert!(
            !screening_tables.contains("vocab_peptides")
                && !screening_tables.contains("unrelated_raw_files")
        );
        assert!(
            reviewed_hla_input_tables(
                &target,
                &partial,
                &json!({"search_type":"protein","search_term":"X"}),
                &[]
            )
            .is_err()
        );
    }

    #[test]
    fn live_roster_and_counts_follow_declared_queries() {
        let target = PluginId::from("alithea/bc-target-validation");
        let roster = "sample_roster.v2".to_owned();
        let count = "sample_observation_counts.v2".to_owned();
        let tables = [
            (
                "obs_samples",
                &["original_sample_number", "sample_class"][..],
            ),
            (
                "obs_sample_peptide_mappings",
                &["original_sample_number"][..],
            ),
        ];
        assert_eq!(
            reviewed_hla_input_tables(
                &target,
                &snapshot(&tables),
                &Value::Null,
                std::slice::from_ref(&roster)
            )
            .unwrap(),
            names(&["obs_samples"])
        );
        assert_eq!(
            reviewed_hla_input_tables(
                &target,
                &snapshot(&tables),
                &Value::Null,
                &[roster, count.clone()]
            )
            .unwrap(),
            names(&["obs_samples", "obs_sample_peptide_mappings"])
        );
        assert!(
            reviewed_hla_input_tables(&target, &snapshot(&tables[..1]), &Value::Null, &[count])
                .is_err()
        );
        assert!(
            reviewed_hla_input_tables(
                &target,
                &snapshot(&tables),
                &Value::Null,
                &["new_read".to_owned()]
            )
            .is_err()
        );
    }
}
