use super::{
    CompassError, CompassResult, OFF_TARGET_TOXICITY_DATASET_INPUT_CONTRACT, PluginId,
    TARGET_VALIDATION_DATASET_INPUT_CONTRACT, TOXICITY_SCREENING_DATASET_INPUT_CONTRACT, Value,
    reviewed_hla_input_contract,
};

/// Check the release validator's capabilities before admitting a module.
pub fn validate_reviewed_hla_capabilities(
    plugin_id: &PluginId,
    caps: &Value,
    params: &Value,
) -> CompassResult<()> {
    let contract = reviewed_hla_input_contract(plugin_id)
        .ok_or_else(|| CompassError::AccessDenied("unsupported private HLA module".into()))?;
    let hla_filter = params["hla_filter"]
        .as_str()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty() && !s.eq_ignore_ascii_case("null"));
    let mut requirements = vec!["sample_roster", "peptide_observations"];
    match contract {
        TARGET_VALIDATION_DATASET_INPUT_CONTRACT => {
            if !matches!(params["search_type"].as_str(), Some("peptide" | "protein"))
                || params["search_term"]
                    .as_str()
                    .is_none_or(|term| term.trim().is_empty())
                || params["max_strip_points"]
                    .as_u64()
                    .is_some_and(|n| n > 5000)
            {
                return Err(CompassError::Validation(
                    "provide a peptide or protein search term and max_strip_points at most 5000"
                        .into(),
                ));
            }
            requirements.push("normalized_intensities");
            if params["search_type"].as_str() == Some("protein") {
                requirements.push("protein_positions");
            }
            if hla_filter {
                requirements.push("hla_typing");
            }
        }
        TOXICITY_SCREENING_DATASET_INPUT_CONTRACT => {
            requirements.push("normalized_intensities");
            if hla_filter {
                requirements.push("hla_typing");
            }
        }
        OFF_TARGET_TOXICITY_DATASET_INPUT_CONTRACT => {
            if params["hla_filters"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
            {
                requirements.push("binding_predictions");
            }
        }
        _ => unreachable!("reviewed HLA contract mapping"),
    }
    let unavailable: Vec<_> = requirements
        .into_iter()
        .filter_map(|name| {
            let capability = &caps[name];
            if capability["available"] == true
                && ["missing", "unpopulated"].iter().all(|key| {
                    capability
                        .get(key)
                        .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty))
                })
            {
                return None;
            }
            let inputs: Vec<_> = ["missing", "unpopulated"]
                .into_iter()
                .flat_map(|key| capability[key].as_array().into_iter().flatten())
                .filter_map(Value::as_str)
                .collect();
            Some(if inputs.is_empty() {
                name.to_owned()
            } else {
                format!("{name} ({})", inputs.join(", "))
            })
        })
        .collect();
    if unavailable.is_empty() {
        return Ok(());
    }
    let name = match contract {
        TARGET_VALIDATION_DATASET_INPUT_CONTRACT => "Target Validation",
        TOXICITY_SCREENING_DATASET_INPUT_CONTRACT => "Toxicity Screening",
        OFF_TARGET_TOXICITY_DATASET_INPUT_CONTRACT => "Off-Target Toxicity",
        _ => unreachable!("reviewed HLA contract mapping"),
    };
    Err(CompassError::Validation(format!(
        "{name} requires {}",
        unavailable.join("; ")
    )))
}
