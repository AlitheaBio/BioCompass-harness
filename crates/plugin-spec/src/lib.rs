//! `biocompass-plugin-spec`: Universal `plugin.toml` manifest parser, `SKILL.md` loader,
//! JSON Schema input validator, and starter template generators.

pub mod templates;

use biocompass_core_traits::{CompassError, CompassResult, CreditAmountAct, OciImageRef, PluginId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub use templates::{TemplateKind, render_template_files, write_template_to_dir};

/// Container network confinement policy declared in `plugin.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NetworkPolicy {
    /// `"none"` (default airgapped container).
    Mode(String),
    /// Explicit domain egress allowlist audited in Quote & Receipt.
    Allowlist { allow_domains: Vec<String> },
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self::Mode("none".into())
    }
}

impl NetworkPolicy {
    pub fn is_airgapped(&self) -> bool {
        matches!(self, Self::Mode(m) if m.trim().eq_ignore_ascii_case("none"))
    }
}

/// Container resource limits enforced by `biocompass-oci-runner`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginResources {
    #[serde(default = "default_cpus")]
    pub cpus: u32,
    #[serde(default = "default_memory_mb")]
    pub memory_mb: u64,
    #[serde(default)]
    pub gpu: u32,
    #[serde(default = "default_timeout_sec")]
    pub timeout_sec: u64,
    #[serde(default = "default_pids_limit")]
    pub pids_limit: u32,
}

const fn default_cpus() -> u32 {
    2
}
const fn default_memory_mb() -> u64 {
    4096
}
const fn default_timeout_sec() -> u64 {
    1800
}
const fn default_pids_limit() -> u32 {
    256
}

impl Default for PluginResources {
    fn default() -> Self {
        Self {
            cpus: default_cpus(),
            memory_mb: default_memory_mb(),
            gpu: 0,
            timeout_sec: default_timeout_sec(),
            pids_limit: default_pids_limit(),
        }
    }
}

/// Input specification wrapping standard JSON Schema (`[inputs.schema]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInputs {
    #[serde(default = "default_input_schema")]
    pub schema: Value,
}

fn default_input_schema() -> Value {
    json!({ "type": "object" })
}

impl Default for PluginInputs {
    fn default() -> Self {
        Self {
            schema: default_input_schema(),
        }
    }
}

/// Declarative scientific result visualization rendered natively by the host SPA.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclarativeViewSpec {
    pub id: String,
    pub kind: String,
    pub title: String,
    /// RFC 6901 JSON Pointer into `/out/result.json` (e.g. `"/length_distribution"`).
    pub data_pointer: String,
}

/// Optional sandboxed Vite UI bundle (`<iframe sandbox="allow-scripts">`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginUiSpec {
    pub entry: String,
}

/// Universal `plugin.toml` manifest (`Plugin = Module = Pipeline`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: PluginId,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_category")]
    pub category: String,
    pub image: String,
    #[serde(default)]
    pub network: NetworkPolicy,
    #[serde(default = "default_price_milli_act")]
    pub price_milli_act: i64,
    #[serde(default)]
    pub resources: PluginResources,
    #[serde(default)]
    pub inputs: PluginInputs,
    #[serde(default)]
    pub views: Vec<DeclarativeViewSpec>,
    #[serde(default)]
    pub ui: Option<PluginUiSpec>,
}

fn default_category() -> String {
    "Bioinformatics".into()
}
const fn default_price_milli_act() -> i64 {
    5_000
}

impl PluginManifest {
    /// Parse and validate a `plugin.toml` document.
    pub fn from_toml_str(raw: &str, allow_local_dev_tags: bool) -> CompassResult<Self> {
        let manifest: Self = toml::from_str(raw)
            .map_err(|e| CompassError::Validation(format!("invalid plugin.toml: {e}")))?;
        manifest.validate(allow_local_dev_tags)?;
        Ok(manifest)
    }

    /// Validate structural invariants, OCI reference rules, and JSON Schema syntax.
    pub fn validate(&self, allow_local_dev_tags: bool) -> CompassResult<OciImageRef> {
        if !self.id.as_str().contains('/') {
            return Err(CompassError::Validation(format!(
                "plugin id '{}' must be namespaced as '<org>/<name>'",
                self.id
            )));
        }
        if self.name.trim().is_empty() {
            return Err(CompassError::Validation("plugin name cannot be empty".into()));
        }
        if self.resources.cpus == 0 || self.resources.memory_mb < 128 {
            return Err(CompassError::Validation(
                "plugin resources require cpus >= 1 and memory_mb >= 128".into(),
            ));
        }
        if let NetworkPolicy::Mode(mode) = &self.network
            && !mode.trim().eq_ignore_ascii_case("none")
        {
            return Err(CompassError::Validation(format!(
                "invalid network mode '{mode}'; use \"none\" or {{ allow_domains = [...] }}"
            )));
        }
        // Compile JSON Schema upfront so broken schemas fail loud at load time.
        jsonschema::validator_for(&self.inputs.schema).map_err(|e| {
            CompassError::Validation(format!("invalid [inputs.schema] JSON Schema: {e}"))
        })?;

        OciImageRef::parse(&self.image, allow_local_dev_tags)
    }

    /// Validate caller `params` JSON against `[inputs.schema]`.
    pub fn validate_inputs(&self, params: &Value) -> CompassResult<()> {
        let validator = jsonschema::validator_for(&self.inputs.schema).map_err(|e| {
            CompassError::Validation(format!("compile [inputs.schema] failed: {e}"))
        })?;
        let errors: Vec<String> = validator
            .iter_errors(params)
            .map(|err| format!("{}: {}", err.instance_path(), err))
            .collect();
        if !errors.is_empty() {
            return Err(CompassError::Validation(format!(
                "input parameters failed schema validation: {}",
                errors.join("; ")
            )));
        }
        Ok(())
    }

    /// Return estimated run cost in `CreditAmountAct`.
    pub const fn estimated_cost_act(&self) -> CreditAmountAct {
        CreditAmountAct::from_milli_act(self.price_milli_act)
    }
}

/// Parsed `SKILL.md` agent guidance accompanying a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDoc {
    pub name: String,
    pub description: String,
    pub body_markdown: String,
}

impl SkillDoc {
    /// Parse a `SKILL.md` file with optional `---` YAML-style frontmatter.
    pub fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();
        if let Some(rest) = trimmed.strip_prefix("---")
            && let Some((front, body)) = rest.split_once("\n---")
        {
            let mut name = String::new();
            let mut description = String::new();
            for line in front.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    match k.trim() {
                        "name" => name = v.trim().trim_matches('"').to_owned(),
                        "description" => description = v.trim().trim_matches('"').to_owned(),
                        _ => {}
                    }
                }
            }
            return Self {
                name,
                description,
                body_markdown: body.trim().to_owned(),
            };
        }
        Self {
            name: "unnamed-skill".into(),
            description: String::new(),
            body_markdown: trimmed.to_owned(),
        }
    }
}

/// Loaded on-disk or registered BioCompass plugin bundle (`plugin.toml` + `SKILL.md` + optional `ui/`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub skill: Option<SkillDoc>,
    pub root_dir: Option<PathBuf>,
}

impl LoadedPlugin {
    /// Load a plugin directory containing `plugin.toml`, optional `SKILL.md`, and optional `ui/`.
    pub fn load_from_dir(dir: impl AsRef<Path>, allow_local_dev_tags: bool) -> CompassResult<Self> {
        let root = dir.as_ref();
        let manifest_path = root.join("plugin.toml");
        if !manifest_path.is_file() {
            return Err(CompassError::NotFound(format!(
                "missing plugin.toml in {}",
                root.display()
            )));
        }
        let raw_toml = std::fs::read_to_string(&manifest_path)
            .map_err(|e| CompassError::Storage(format!("read {}: {e}", manifest_path.display())))?;
        let manifest = PluginManifest::from_toml_str(&raw_toml, allow_local_dev_tags)?;

        let skill_path = root.join("SKILL.md");
        let skill = if skill_path.is_file() {
            let raw_md = std::fs::read_to_string(&skill_path).map_err(|e| {
                CompassError::Storage(format!("read {}: {e}", skill_path.display()))
            })?;
            Some(SkillDoc::parse(&raw_md))
        } else {
            None
        };

        Ok(Self {
            manifest,
            skill,
            root_dir: Some(root.to_path_buf()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffolds_and_validates_all_four_starter_templates() {
        for kind in [
            TemplateKind::PythonUv,
            TemplateKind::RBioc,
            TemplateKind::Nextflow,
            TemplateKind::RustCli,
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let written = write_template_to_dir(
                kind,
                "alithea/hla-starter",
                "HLA Starter Module",
                true,
                dir.path(),
            )
            .expect("scaffold template");
            assert!(written.contains(&"plugin.toml".to_string()));
            assert!(written.contains(&"Dockerfile".to_string()));
            assert!(written.contains(&"SKILL.md".to_string()));
            assert!(written.contains(&"ui/dist/index.html".to_string()));

            let loaded =
                LoadedPlugin::load_from_dir(dir.path(), true).expect("load scaffolded plugin");
            assert_eq!(loaded.manifest.id.as_str(), "alithea/hla-starter");
            assert!(loaded.skill.is_some());

            // Valid input passes JSON Schema
            loaded
                .manifest
                .validate_inputs(&json!({
                    "alleles": ["HLA-A*02:01"],
                    "min_length": 8,
                    "max_length": 11
                }))
                .expect("valid params");

            // Out-of-bounds min_length fails JSON Schema
            let bad = loaded.manifest.validate_inputs(&json!({
                "alleles": ["HLA-A*02:01"],
                "min_length": 3,
                "max_length": 11
            }));
            assert!(bad.is_err(), "min_length=3 must fail schema minimum=8");
        }
    }
}
