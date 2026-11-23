use super::{CompassError, CompassResult, PluginManifest};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum VersionOperationDeclaration {
    Named(String),
    Described(VersionOperationSpec),
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VersionInputKind {
    ValidatedUpload,
    PriorRelease,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VersionOperationSpec {
    pub id: String,
    pub title: String,
    pub parameters: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_input: Option<VersionInputKind>,
}

fn invalid(problem: &str) -> CompassError {
    CompassError::Validation(problem.into())
}

fn identifier(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn reserved_field(name: &str) -> bool {
    matches!(
        name,
        "kind" | "operation" | "bundle_filename" | "corrections"
    )
}

/// Construct the OCI invocation under the same bounds at admission and at the worker.
pub fn version_operation_params(kind: &str, fields: &Map<String, Value>) -> CompassResult<Value> {
    if !identifier(kind) || matches!(kind, "validate" | "import") {
        return Err(invalid("invalid or reserved dataset version action"));
    }
    if fields.len() > 16
        || fields.iter().any(|(name, value)| {
            !identifier(name)
                || reserved_field(name)
                || match value {
                    Value::String(s) => s.len() > 4096 || s.contains('\0'),
                    Value::Bool(_) | Value::Number(_) => false,
                    _ => true,
                }
        })
    {
        return Err(invalid("action inputs must be bounded scalar values"));
    }
    let mut params = fields.clone();
    params.insert("operation".into(), Value::String(kind.to_owned()));
    let params = Value::Object(params);
    if params.to_string().len() > 16 * 1024 {
        return Err(invalid("action input exceeds its size limit"));
    }
    Ok(params)
}

impl VersionOperationSpec {
    fn validate(&self) -> CompassResult<()> {
        if !identifier(&self.id) || matches!(self.id.as_str(), "import" | "validate") {
            return Err(invalid("invalid or reserved dataset version action"));
        }
        if self.title.trim().is_empty()
            || self.title.len() > 160
            || self.title.chars().any(char::is_control)
        {
            return Err(invalid("invalid dataset version action title"));
        }
        let schema = &self.parameters;
        let properties = schema["properties"]
            .as_object()
            .ok_or_else(|| invalid("action parameters require named properties"))?;
        let object = schema.as_object().expect("properties on an object");
        if properties.len() > 16
            || schema["type"] != "object"
            || schema["additionalProperties"] != false
            || object.keys().any(|k| {
                !matches!(
                    k.as_str(),
                    "type" | "properties" | "required" | "additionalProperties"
                )
            })
            || schema.to_string().len() > 16 * 1024
        {
            return Err(invalid("action parameters must be a bounded closed object"));
        }
        if let Some(required) = schema.get("required") {
            let required = required
                .as_array()
                .ok_or_else(|| invalid("invalid required fields"))?;
            if required
                .iter()
                .any(|v| v.as_str().is_none_or(|name| !properties.contains_key(name)))
            {
                return Err(invalid("unknown required action field"));
            }
        }
        for (name, property) in properties {
            let field = property
                .as_object()
                .ok_or_else(|| invalid("invalid action field schema"))?;
            if !identifier(name)
                || reserved_field(name)
                || !matches!(
                    property["type"].as_str(),
                    Some("string" | "integer" | "number" | "boolean")
                )
                || field.keys().any(|k| {
                    !matches!(
                        k.as_str(),
                        "type"
                            | "title"
                            | "description"
                            | "default"
                            | "enum"
                            | "minLength"
                            | "maxLength"
                            | "pattern"
                            | "minimum"
                            | "maximum"
                            | "exclusiveMinimum"
                            | "exclusiveMaximum"
                            | "multipleOf"
                    )
                })
            {
                return Err(invalid("action fields must be named scalar inputs"));
            }
        }
        jsonschema::validator_for(schema)
            .map_err(|_| invalid("invalid action parameter schema"))?;
        Ok(())
    }
}

impl PluginManifest {
    pub fn version_operation_specs(&self) -> CompassResult<Vec<VersionOperationSpec>> {
        let contract = self.dataset_import_contract()?;
        if contract.version_operations.len() > 16 {
            return Err(invalid("too many dataset version actions"));
        }
        let mut actions = Vec::new();
        let mut ids = BTreeSet::new();
        for entry in contract.version_operations {
            let action = match entry {
                VersionOperationDeclaration::Described(spec) => spec,
                VersionOperationDeclaration::Named(id) => {
                    // An older manifest named actions and declared its fields on the invocation itself.
                    let mut fields = self.inputs.schema["properties"]
                        .as_object()
                        .cloned()
                        .unwrap_or_default();
                    fields.retain(|name, _| !reserved_field(name));
                    let required: Vec<_> = self.inputs.schema["required"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|value| {
                            value.as_str().is_some_and(|name| fields.contains_key(name))
                        })
                        .collect();
                    let parameters = json!({"type":"object","properties":fields,"required":required,"additionalProperties":false});
                    VersionOperationSpec {
                        title: id.replace('_', " "),
                        id,
                        parameters,
                        secondary_input: None,
                    }
                }
            };
            action.validate()?;
            if !ids.insert(action.id.clone()) {
                return Err(invalid("duplicate dataset version action"));
            }
            actions.push(action);
        }
        Ok(actions)
    }

    pub fn validate_version_operation(&self, operation: &Value) -> CompassResult<()> {
        self.validate_version_operation_spec(operation).map(|_| ())
    }

    pub fn validate_version_operation_spec(
        &self,
        operation: &Value,
    ) -> CompassResult<VersionOperationSpec> {
        let mut fields: Map<String, Value> = operation
            .as_object()
            .cloned()
            .ok_or_else(|| invalid("expected a dataset version action"))?;
        let kind = fields
            .remove("kind")
            .and_then(|v| v.as_str().map(str::to_owned))
            .ok_or_else(|| invalid("missing action identifier"))?;
        let params = version_operation_params(&kind, &fields)?;
        let spec = self
            .version_operation_specs()?
            .into_iter()
            .find(|s| s.id == kind)
            .ok_or_else(|| invalid("this importer does not offer the requested action"))?;
        let validator = jsonschema::validator_for(&spec.parameters)
            .map_err(|_| invalid("invalid action parameter schema"))?;
        if let Err(err) = validator.validate(&Value::Object(fields)) {
            return Err(CompassError::Validation(format!(
                "action inputs do not match its declaration at {}",
                err.instance_path()
            )));
        }
        self.validate_inputs(&params).map_err(|_| {
            invalid("action inputs do not match the importer's invocation contract")
        })?;
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn importer() -> PluginManifest {
        serde_json::from_value(json!({
            "id":"qa/notes", "name":"Notes", "version":"1.1.0", "image":"qa/notes:1.1.0",
            "inputs":{"schema":{"type":"object","additionalProperties":false,"properties":{
                "operation":{"type":"string","enum":["import","set_label"]},"label":{"type":"string"}
            },"x-biocompass-import":{
                "contract":"biocompass.dataset-import.v1","accept":["*"],
                "version_operations":[{"id":"set_label","title":"Set label","parameters":{
                    "type":"object","additionalProperties":false,"properties":{"label":{"type":"string","minLength":2}},"required":["label"]
                }}]
            }}}
        })).unwrap()
    }

    #[test]
    fn action_accepts_only_declared_inputs_for_a_reviewed_importer() {
        let importer = importer();
        assert!(
            importer
                .validate_version_operation(&json!({"kind":"set_label","label":"two"}))
                .is_ok()
        );
        for invalid in [
            json!({"kind":"set_label","label":"x"}),
            json!({"kind":"set_label","label":"two","other":3}),
            json!({"kind":"import"}),
            json!({"kind":"set_label","label":"x".repeat(4097)}),
            json!({"kind":"set_label","label":{"value":"two"}}),
        ] {
            assert!(importer.validate_version_operation(&invalid).is_err());
        }
    }

    #[test]
    fn signed_secondary_source_is_independent_of_scalar_parameters() {
        let mut importer = importer();
        let index = 0;
        let action = json!({"kind":"set_label","label":"approved"});
        assert!(
            importer
                .validate_version_operation_spec(&action)
                .unwrap()
                .secondary_input
                .is_none()
        );
        for (kind, expected) in [
            ("validated_upload", VersionInputKind::ValidatedUpload),
            ("prior_release", VersionInputKind::PriorRelease),
        ] {
            importer.inputs.schema["x-biocompass-import"]["version_operations"][index]["secondary_input"] =
                json!(kind);
            assert_eq!(
                importer
                    .validate_version_operation_spec(&action)
                    .unwrap()
                    .secondary_input,
                Some(expected)
            );
        }
        importer.inputs.schema["x-biocompass-import"]["version_operations"][index]["secondary_input"] =
            json!("external_path");
        assert!(importer.validate_version_operation_spec(&action).is_err());
    }

    #[test]
    fn existing_named_actions_use_their_signed_invocation_fields() {
        let mut importer = importer();
        importer.inputs.schema["x-biocompass-import"]["version_operations"] = json!(["set_label"]);
        assert!(
            importer
                .validate_version_operation(&json!({"kind":"set_label","label":"one"}))
                .is_ok()
        );
        assert!(
            importer
                .validate_version_operation(
                    &json!({"kind":"set_label","bundle_filename":"private.zip"})
                )
                .is_err()
        );
        importer.inputs.schema["x-biocompass-import"]
            .as_object_mut()
            .unwrap()
            .remove("version_operations");
        assert!(importer.version_operation_specs().unwrap().is_empty());
    }
}
