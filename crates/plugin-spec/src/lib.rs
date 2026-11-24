//! `biocompass-plugin-spec`: Universal `plugin.toml` manifest parser, `SKILL.md` loader,
//! JSON Schema input validator, and starter template generators.

pub mod templates;
mod version_operations;
mod hla_capabilities;
pub use hla_capabilities::validate_reviewed_hla_capabilities;
pub use version_operations::{VersionInputKind, VersionOperationDeclaration, VersionOperationSpec, version_operation_params};

use biocompass_core_traits::{
    CompassError, CompassResult, CreditAmountAct, GrantLevel, OciImageRef, PluginId, ScopedQueryDefinition,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub use templates::{
    TemplateKind, render_template_files, validate_scaffold_plugin_id, write_template_to_dir,
};

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

/// Allowed execution plane(s) declared in `plugin.toml` (§5.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginExecutionPlane {
    Local,
    CloudOnly,
    #[default]
    Either,
}

impl PluginExecutionPlane {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::CloudOnly => "cloud_only",
            Self::Either => "either",
        }
    }
}

/// Optional sandboxed Vite UI bundle (`<iframe sandbox="allow-scripts">`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginUiSpec {
    pub entry: String,
}

const fn default_offer_max_input_bytes() -> u64 {
    10 * 1024 * 1024 // 10 MiB canonical params.json ceiling by default
}

const fn default_offer_max_output_bytes() -> u64 {
    256 * 1024 * 1024 // 256 MiB /out + stdout/stderr ceiling by default
}

/// Fixed-price execution Offer declared under `[[offers]]` in `plugin.toml` (§6.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginOffer {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub price_milli_act: i64,
    #[serde(default)]
    pub base_fee_milli_act: i64,
    #[serde(default = "default_offer_max_input_bytes")]
    pub max_input_bytes: u64,
    #[serde(default = "default_offer_max_output_bytes")]
    pub max_output_bytes: u64,
    #[serde(default)]
    pub max_runtime_sec: Option<u64>,
    #[serde(default)]
    pub max_memory_mb: Option<u64>,
}

/// Fully resolved Offer with concrete execution limits and pricing (§6.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPluginOffer {
    pub id: String,
    pub name: String,
    pub price_milli_act: i64,
    pub base_fee_milli_act: i64,
    pub max_input_bytes: u64,
    pub max_output_bytes: u64,
    pub max_runtime_sec: u64,
    pub max_memory_mb: u64,
}

impl ResolvedPluginOffer {
    pub const fn total_milli_act(&self) -> i64 {
        self.base_fee_milli_act.saturating_add(self.price_milli_act)
    }

    pub const fn total_cost_act(&self) -> CreditAmountAct {
        CreditAmountAct::from_milli_act(self.total_milli_act())
    }

    pub const fn base_fee_act(&self) -> CreditAmountAct {
        CreditAmountAct::from_milli_act(self.base_fee_milli_act)
    }
}

/// Maximum total uncompressed size of a module's UI bundle across client and hub (`U1`, 32 MiB).
pub const MAX_UI_BUNDLE_BYTES: u64 = 32 * 1024 * 1024;
/// Maximum number of allowed child module slugs declared in `plugin.toml` (`W7`).
pub const MAX_CHILD_MODULES: usize = 32;
/// Maximum cumulative wait time (seconds) a parent module may pause while awaiting child runs (`W7`, 24 h).
pub const MAX_CHILD_WAIT_SEC: u64 = 86_400;
/// Platform default child spend cap per parent run when the manifest omits one (`W7`, 10.00 EUR).
pub const DEFAULT_MAX_CHILD_SPEND_MILLI_ACT: i64 = 10_000;
/// Upper bound accepted for a manifest `max_child_spend_milli_act` (`W7`, 10,000.00 EUR).
pub const MAX_CHILD_SPEND_MILLI_ACT: i64 = 10_000_000;
/// Manifest keyword for [`ChildModules::Caller`].
pub const CHILD_MODULES_CALLER: &str = "caller";

/// Which child modules a parent module may start through the file mailbox (`W7`).
///
/// `List` is an explicit allow-list; `Caller` lets the module start any module the initiating
/// principal (the parent run's agent credential, otherwise the person who confirmed the parent)
/// could prepare and confirm directly. The hub authorises every child as that principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildModules {
    List(Vec<String>),
    Caller,
}

impl Default for ChildModules {
    fn default() -> Self {
        Self::List(Vec::new())
    }
}

impl From<Vec<String>> for ChildModules {
    fn from(list: Vec<String>) -> Self {
        Self::List(list)
    }
}

impl ChildModules {
    /// `true` when the module declares no child modules at all (mailbox child runs disabled).
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::List(list) if list.is_empty())
    }

    /// `true` when this module may start child runs (any list entry or `"caller"`).
    pub fn declares_children(&self) -> bool {
        !self.is_empty()
    }

    pub fn is_caller(&self) -> bool {
        matches!(self, Self::Caller)
    }

    /// Module-level policy check: `Caller` permits every module (the hub authorises the principal);
    /// an explicit list permits only listed IDs.
    pub fn permits(&self, plugin_id: &str) -> bool {
        match self {
            Self::Caller => true,
            Self::List(list) => list.iter().any(|allowed| allowed == plugin_id),
        }
    }

    /// Explicit list entries (empty for `Caller`).
    pub fn listed(&self) -> &[String] {
        match self {
            Self::List(list) => list,
            Self::Caller => &[],
        }
    }

    /// JSON form used in signed terms and in `/in/.bc/config.json`.
    pub fn to_json(&self) -> Value {
        match self {
            Self::List(list) => json!(list),
            Self::Caller => json!(CHILD_MODULES_CALLER),
        }
    }
}

impl Serialize for ChildModules {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::List(list) => list.serialize(serializer),
            Self::Caller => serializer.serialize_str(CHILD_MODULES_CALLER),
        }
    }
}

impl<'de> Deserialize<'de> for ChildModules {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            List(Vec<String>),
            Keyword(String),
        }
        match Raw::deserialize(deserializer)? {
            Raw::List(list) => Ok(Self::List(list)),
            Raw::Keyword(word) if word == CHILD_MODULES_CALLER => Ok(Self::Caller),
            Raw::Keyword(other) => Err(serde::de::Error::custom(format!(
                "child_modules must be a list of module IDs or \"{CHILD_MODULES_CALLER}\", got \"{other}\""
            ))),
        }
    }
}

pub const TARGET_VALIDATION_DATASET_INPUT_CONTRACT: &str = "biocompass.target-validation.v1";
pub const OFF_TARGET_TOXICITY_DATASET_INPUT_CONTRACT: &str = "biocompass.off-target-toxicity.v1";
pub const TOXICITY_SCREENING_DATASET_INPUT_CONTRACT: &str = "biocompass.toxicity-screening.v1";

/// Named, provider-owned inputs supported by the reviewed HLA module releases.
pub fn reviewed_hla_input_contract(plugin_id: &PluginId) -> Option<&'static str> {
    match plugin_id.as_str() {
        "alithea/bc-target-validation" => Some(TARGET_VALIDATION_DATASET_INPUT_CONTRACT),
        "alithea/bc-off-target-toxicity" => Some(OFF_TARGET_TOXICITY_DATASET_INPUT_CONTRACT),
        "alithea/bc-toxicity-screening" => Some(TOXICITY_SCREENING_DATASET_INPUT_CONTRACT),
        _ => None,
    }
}

/// Universal `plugin.toml` manifest (`Plugin = Module = Pipeline`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: PluginId,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_category")]
    pub category: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub execution: PluginExecutionPlane,
    #[serde(default)]
    pub requires_catalog_level: GrantLevel,
    /// Named contract for trusted dataset projections supplied by the runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset_input_contract: Option<String>,
    /// Provider-owned, versioned reads this reviewed module can invoke while running.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_queries: Vec<String>,
    /// Named measurement aggregates, optionally filtered through an approved release relationship.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub runtime_join_queries: std::collections::BTreeMap<String, ScopedQueryDefinition>,
    /// Child modules this module may start via the child-run broker (`W7`): an explicit list of
    /// module IDs (`<org>/<name>`) or `"caller"` (any module the initiating principal may start).
    #[serde(default, skip_serializing_if = "ChildModules::is_empty")]
    pub child_modules: ChildModules,
    /// Maximum cumulative seconds this parent module may pause its execution clock waiting for child runs (`W7`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_child_wait_sec: Option<u64>,
    /// Signed cap (milli-EUR) on the total spend of all children of one parent run (`W7`).
    /// When omitted the platform default ([`DEFAULT_MAX_CHILD_SPEND_MILLI_ACT`]) applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_child_spend_milli_act: Option<i64>,
    pub image: String,
    #[serde(default)]
    pub network: NetworkPolicy,
    #[serde(default = "default_price_milli_act")]
    pub price_milli_act: i64,
    #[serde(default)]
    pub offers: Vec<PluginOffer>,
    #[serde(default)]
    pub resources: PluginResources,
    #[serde(default)]
    pub inputs: PluginInputs,
    #[serde(default)]
    pub views: Vec<DeclarativeViewSpec>,
    #[serde(default)]
    pub ui: Option<PluginUiSpec>,
    #[serde(default)]
    pub documentation_url: Option<String>,
    #[serde(default)]
    pub video_url: Option<String>,
    #[serde(default)]
    pub publication_url: Option<String>,
}

fn default_category() -> String {
    "Bioinformatics".into()
}
const fn default_price_milli_act() -> i64 {
    5_000
}

/// The versioned resource materialization contract carried by an importer manifest.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DatasetImportContract {
    pub contract: String,
    pub accept: Vec<String>,
    #[serde(default)]
    pub row_corrections: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub version_operations: Vec<VersionOperationDeclaration>,
}

impl DatasetImportContract {
    pub fn accepts(&self, filename: &str) -> bool {
        let filename = filename.to_ascii_lowercase();
        self.accept.iter().any(|suffix| suffix == "*" || filename.ends_with(&suffix.to_ascii_lowercase()))
    }
}

impl PluginManifest {
    pub fn has_live_queries(&self) -> bool {
        !self.runtime_queries.is_empty() || !self.runtime_join_queries.is_empty()
    }

    /// Signed per-parent child spend cap: the manifest value, or the platform default when the
    /// module declares child modules; `None` for modules that cannot start children (`W7`).
    pub fn effective_max_child_spend_milli_act(&self) -> Option<i64> {
        self.child_modules
            .declares_children()
            .then(|| self.max_child_spend_milli_act.unwrap_or(DEFAULT_MAX_CHILD_SPEND_MILLI_ACT))
    }

    /// Modules with an explicit provider input contract require an authorized dataset at quote time.
    pub fn validate_dataset_binding(&self, has_grant: bool) -> CompassResult<()> {
        if !has_grant && self.dataset_input_contract.is_some() {
            return Err(CompassError::Validation(format!(
                "module '{}' requires an authorized dataset for its declared input contract", self.id
            )));
        }
        Ok(())
    }

    pub fn dataset_import_contract(&self) -> CompassResult<DatasetImportContract> {
        let value = self.inputs.schema.get("x-biocompass-import")
            .ok_or_else(|| CompassError::Validation("module has no import contract".into()))?;
        let contract: DatasetImportContract = serde_json::from_value(value.clone())
            .map_err(|_| CompassError::Validation("invalid import declaration".into()))?;
        if contract.contract != "biocompass.dataset-import.v1" || contract.accept.is_empty()
            || !contract.accept.iter().all(|suffix| suffix == "*"
                || suffix.strip_prefix('.').is_some_and(|ext| ext.split('.').all(|part|
                    !part.is_empty() && part.bytes().all(|ch| ch.is_ascii_alphanumeric()))))
        {
            return Err(CompassError::Validation("unsupported importer file contract".into()));
        }
        Ok(contract)
    }

    /// Parse a `plugin.toml` document without running `validate(...)` yet (used by `publish-plugin`
    /// before building/pushing and pinning `@sha256`, followed by `manifest.validate(false)`).
    pub fn from_toml_unvalidated(raw: &str) -> CompassResult<Self> {
        toml::from_str(raw)
            .map_err(|e| CompassError::Validation(format!("invalid plugin.toml: {e}")))
    }

    /// Parse and validate a `plugin.toml` document.
    pub fn from_toml_str(raw: &str, allow_local_dev_tags: bool) -> CompassResult<Self> {
        let manifest = Self::from_toml_unvalidated(raw)?;
        manifest.validate(allow_local_dev_tags)?;
        Ok(manifest)
    }

    /// Parse and validate a source-tree `plugin.toml` document (allowing canonical unpinned
    /// `<registry>/<repo>:<tag>` refs before CI stamps `@sha256:<64-hex>` into the published manifest).
    pub fn from_toml_source_str(raw: &str) -> CompassResult<Self> {
        let manifest = Self::from_toml_unvalidated(raw)?;
        manifest.validate_source()?;
        Ok(manifest)
    }

    /// Validate structural invariants, OCI reference rules, and JSON Schema syntax.
    pub fn validate(&self, allow_local_dev_tags: bool) -> CompassResult<OciImageRef> {
        self.validate_structure()?;
        OciImageRef::parse(&self.image, allow_local_dev_tags)
    }

    /// Validate structural invariants and JSON Schema for a source-tree `plugin.toml`
    /// whose `image` carries a canonical tag before CI stamps an immutable `@sha256:` digest.
    pub fn validate_source(&self) -> CompassResult<OciImageRef> {
        self.validate_structure()?;
        OciImageRef::parse_source_manifest_ref(&self.image)
    }

    fn validate_structure(&self) -> CompassResult<()> {
        if !self.id.as_str().contains('/') {
            return Err(CompassError::Validation(format!(
                "plugin id '{}' must be namespaced as '<org>/<name>'",
                self.id
            )));
        }
        if self.name.trim().is_empty() {
            return Err(CompassError::Validation("plugin name cannot be empty".into()));
        }
        if self.resources.cpus == 0 || self.resources.memory_mb < 64 {
            return Err(CompassError::Validation(
                "plugin resources require cpus >= 1 and memory_mb >= 64".into(),
            ));
        }
        if self.price_milli_act < 0 {
            return Err(CompassError::Validation(
                "plugin price_milli_act cannot be negative".into(),
            ));
        }
        if !self.offers.is_empty() {
            let mut seen_ids = std::collections::HashSet::new();
            for offer in &self.offers {
                let oid = offer.id.trim();
                if oid.is_empty()
                    || oid.len() > 64
                    || !oid
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                {
                    return Err(CompassError::Validation(format!(
                        "invalid offer id '{}'; must be 1..=64 ASCII [a-zA-Z0-9._-] chars",
                        offer.id
                    )));
                }
                if !seen_ids.insert(oid.to_owned()) {
                    return Err(CompassError::Validation(format!(
                        "duplicate offer id '{oid}' in plugin.toml"
                    )));
                }
                if offer.price_milli_act < 0 || offer.base_fee_milli_act < 0 {
                    return Err(CompassError::Validation(format!(
                        "offer '{oid}' cannot have negative price_milli_act or base_fee_milli_act"
                    )));
                }
                let runtime_sec = offer.max_runtime_sec.unwrap_or(self.resources.timeout_sec);
                let memory_mb = offer.max_memory_mb.unwrap_or(self.resources.memory_mb);
                if offer.max_input_bytes < 64
                    || offer.max_output_bytes < 256
                    || runtime_sec == 0
                    || memory_mb < 64
                {
                    return Err(CompassError::Validation(format!(
                        "offer '{oid}' limits out of bounds (require max_input_bytes >= 64, max_output_bytes >= 256, max_runtime_sec >= 1, max_memory_mb >= 64)"
                    )));
                }
            }
        }
        if let NetworkPolicy::Mode(mode) = &self.network
            && !mode.trim().eq_ignore_ascii_case("none")
        {
            return Err(CompassError::Validation(format!(
                "invalid network mode '{mode}'; use \"none\" or {{ allow_domains = [...] }}"
            )));
        }
        if let Some(contract) = self.dataset_input_contract.as_deref()
            && (Some(contract) != reviewed_hla_input_contract(&self.id)
                || !self.network.is_airgapped())
        {
            return Err(CompassError::Validation(format!(
                "unsupported dataset input contract '{contract}' for plugin '{}'",
                self.id
            )));
        }
        if !self.runtime_queries.is_empty()
            && (self.dataset_input_contract.is_none()
                || self.runtime_queries.len() > 16
                || self.runtime_queries.iter().enumerate().any(|(index, query)| {
                    query.is_empty()
                        || query.len() > 96
                        || !query.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-'))
                        || self.runtime_queries[..index].contains(query)
                }))
        {
            return Err(CompassError::Validation(
                "runtime_queries requires a reviewed input contract and up to 16 unique query IDs".into(),
            ));
        }
        if !self.runtime_join_queries.is_empty()
            && (!self.runtime_queries.is_empty()
                || self.dataset_input_contract.is_some()
                || self.requires_catalog_level != GrantLevel::ComputeOnly
                || !self.network.is_airgapped()
                || self.runtime_join_queries.len() > 16
                || self.runtime_join_queries.iter().any(|(query, definition)| {
                    query.is_empty()
                        || query.len() > 96
                        || !query.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-'))
                        || definition.measure().is_empty()
                        || definition.path().is_some_and(|path| {
                            path.is_empty() || path.len() > 128
                                || !(path.as_bytes()[0].is_ascii_alphabetic() || path.starts_with('_'))
                                || !path.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                        })
                }))
        {
            return Err(CompassError::Validation(
                "runtime_join_queries requires an isolated compute-only module and up to 16 named measures".into(),
            ));
        }
        if let ChildModules::List(children) = &self.child_modules {
            if children.len() > MAX_CHILD_MODULES {
                return Err(CompassError::Validation(format!(
                    "child_modules may declare at most {MAX_CHILD_MODULES} modules"
                )));
            }
            let mut seen_children = std::collections::HashSet::new();
            for child in children {
                validate_scaffold_plugin_id(child).map_err(|_| {
                    CompassError::Validation(format!(
                        "invalid child_modules entry '{child}': must be '<namespace>/<slug>' matching ^[a-z0-9][a-z0-9_-]{{0,62}}$"
                    ))
                })?;
                if child == self.id.as_str() {
                    return Err(CompassError::Validation(format!(
                        "child_modules cannot include self ('{}')",
                        self.id
                    )));
                }
                if !seen_children.insert(child.as_str()) {
                    return Err(CompassError::Validation(format!(
                        "duplicate child_modules entry '{child}'"
                    )));
                }
            }
        }
        if let Some(wait_sec) = self.max_child_wait_sec
            && (wait_sec == 0 || wait_sec > MAX_CHILD_WAIT_SEC)
        {
            return Err(CompassError::Validation(format!(
                "max_child_wait_sec must be between 1 and {MAX_CHILD_WAIT_SEC} seconds, got {wait_sec}"
            )));
        }
        if let Some(cap) = self.max_child_spend_milli_act
            && !(0..=MAX_CHILD_SPEND_MILLI_ACT).contains(&cap)
        {
            return Err(CompassError::Validation(format!(
                "max_child_spend_milli_act must be between 0 and {MAX_CHILD_SPEND_MILLI_ACT}, got {cap}"
            )));
        }
        if self.max_child_spend_milli_act.is_some() && self.child_modules.is_empty() {
            return Err(CompassError::Validation(
                "max_child_spend_milli_act requires child_modules".into(),
            ));
        }
        if self.inputs.schema.get("x-biocompass-import").is_some() {
            self.version_operation_specs()?;
        }
        // Compile JSON Schema upfront so broken schemas fail loud at load time.
        jsonschema::validator_for(&self.inputs.schema).map_err(|e| {
            CompassError::Validation(format!("invalid [inputs.schema] JSON Schema: {e}"))
        })?;

        Ok(())
    }

    /// Return the effective Offer list (`[[offers]]` if declared, or 1 synthesized default Offer
    /// from top-level `price_milli_act` and `[resources]`, §6.2).
    pub fn effective_offers(&self) -> Vec<ResolvedPluginOffer> {
        if self.offers.is_empty() {
            return vec![ResolvedPluginOffer {
                id: "default".into(),
                name: "Standard Run".into(),
                price_milli_act: self.price_milli_act,
                base_fee_milli_act: 0,
                max_input_bytes: default_offer_max_input_bytes(),
                max_output_bytes: default_offer_max_output_bytes(),
                max_runtime_sec: self.resources.timeout_sec.max(1),
                max_memory_mb: self.resources.memory_mb.max(64),
            }];
        }
        self.offers
            .iter()
            .map(|o| {
                let id = o.id.trim().to_owned();
                let name = if o.name.trim().is_empty() {
                    id.clone()
                } else {
                    o.name.trim().to_owned()
                };
                ResolvedPluginOffer {
                    id,
                    name,
                    price_milli_act: o.price_milli_act,
                    base_fee_milli_act: o.base_fee_milli_act,
                    max_input_bytes: o.max_input_bytes,
                    max_output_bytes: o.max_output_bytes,
                    max_runtime_sec: o.max_runtime_sec.unwrap_or(self.resources.timeout_sec).max(1),
                    max_memory_mb: o.max_memory_mb.unwrap_or(self.resources.memory_mb).max(64),
                }
            })
            .collect()
    }

    /// Resolve a requested `offer_id` (or the default first Offer when `None`).
    pub fn resolve_offer(&self, offer_id: Option<&str>) -> CompassResult<ResolvedPluginOffer> {
        let offers = self.effective_offers();
        match offer_id.map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(offers[0].clone()),
            Some(wanted) => offers
                .into_iter()
                .find(|o| o.id == wanted)
                .ok_or_else(|| {
                    CompassError::Validation(format!(
                        "offer '{wanted}' does not exist on plugin '{}'",
                        self.id
                    ))
                }),
        }
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

    /// Return default estimated run cost in `CreditAmountAct`.
    pub fn estimated_cost_act(&self) -> CreditAmountAct {
        self.effective_offers()[0].total_cost_act()
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
        Self::load_from_dir_with(dir, |raw| {
            PluginManifest::from_toml_str(raw, allow_local_dev_tags)
        })
    }

    /// Load a source-tree plugin directory whose `plugin.toml` may carry an unpinned canonical tag
    /// (`ghcr.io/alitheabio/<module>:<version>`) before CI stamps `@sha256:` into the published manifest.
    pub fn load_source_from_dir(dir: impl AsRef<Path>) -> CompassResult<Self> {
        Self::load_from_dir_with(dir, PluginManifest::from_toml_source_str)
    }

    fn load_from_dir_with(
        dir: impl AsRef<Path>,
        parse_manifest: impl FnOnce(&str) -> CompassResult<PluginManifest>,
    ) -> CompassResult<Self> {
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
        let manifest = parse_manifest(&raw_toml)?;

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
            let target = dir.path().join("hla-starter");
            let written = write_template_to_dir(
                kind,
                "alithea/hla-starter",
                "HLA Starter Module",
                true,
                &target,
            )
            .expect("scaffold template");
            assert!(written.contains(&"plugin.toml".to_string()));
            assert!(written.contains(&"Dockerfile".to_string()));
            assert!(written.contains(&"SKILL.md".to_string()));
            assert!(written.contains(&"ui/dist/index.html".to_string()));

            let loaded =
                LoadedPlugin::load_from_dir(&target, true).expect("load scaffolded plugin");
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

            // Legacy manifest without [[offers]] synthesizes 1 default offer
            let default_offer = loaded
                .manifest
                .resolve_offer(None)
                .expect("default synthesized offer");
            assert_eq!(default_offer.id, "default");
            assert_eq!(default_offer.price_milli_act, loaded.manifest.price_milli_act);
            assert_eq!(default_offer.base_fee_milli_act, 0);
        }
    }

    #[test]
    fn parses_multiple_offers_and_rejects_duplicate_ids() {
        let toml_doc = r#"
id = "alithea/multi-offer"
name = "Multi Offer Test"
version = "1.0.0"
image = "local/multi-offer:dev"

[resources]
cpus = 2
memory_mb = 2048
timeout_sec = 300

[[offers]]
id = "quick-scan"
name = "Quick Scan"
price_milli_act = 2500
base_fee_milli_act = 500
max_input_bytes = 4096
max_output_bytes = 1048576
max_runtime_sec = 30
max_memory_mb = 256

[[offers]]
id = "deep-cohort"
name = "Deep Cohort Run"
price_milli_act = 15000
max_input_bytes = 65536
max_output_bytes = 67108864
"#;
        let manifest = PluginManifest::from_toml_str(toml_doc, true).expect("parse multi-offer");
        assert_eq!(manifest.effective_offers().len(), 2);

        let quick = manifest
            .resolve_offer(Some("quick-scan"))
            .expect("resolve quick-scan");
        assert_eq!(quick.total_milli_act(), 3_000);
        assert_eq!(quick.max_runtime_sec, 30);
        assert_eq!(quick.max_memory_mb, 256);

        let deep = manifest
            .resolve_offer(Some("deep-cohort"))
            .expect("resolve deep-cohort");
        assert_eq!(deep.total_milli_act(), 15_000);
        assert_eq!(deep.max_runtime_sec, 300);
        assert_eq!(deep.max_memory_mb, 2048);

        assert!(manifest.resolve_offer(Some("nonexistent")).is_err());

        let dup_toml = format!(
            "{toml_doc}\n[[offers]]\nid = \"quick-scan\"\nprice_milli_act = 1000\n"
        );
        assert!(PluginManifest::from_toml_str(&dup_toml, true).is_err());
    }

    #[test]
    fn reviewed_hla_contract_belongs_to_its_network_isolated_module() {
        for (id, contract) in [
            ("alithea/bc-target-validation", TARGET_VALIDATION_DATASET_INPUT_CONTRACT),
            ("alithea/bc-off-target-toxicity", OFF_TARGET_TOXICITY_DATASET_INPUT_CONTRACT),
            ("alithea/bc-toxicity-screening", TOXICITY_SCREENING_DATASET_INPUT_CONTRACT),
        ] {
            let manifest = |id: &str, contract: &str| format!(
                "id = \"{id}\"\nname = \"Test\"\nversion = \"1.0.0\"\nnetwork = \"none\"\nimage = \"local/test:dev\"\ndataset_input_contract = \"{contract}\"\n"
            );
            assert!(PluginManifest::from_toml_str(&manifest(id, contract), true).is_ok());
            assert!(PluginManifest::from_toml_str(&manifest("fixture/foreign", contract), true).is_err());
        }
        let wrong_contract = r#"
id = "alithea/bc-target-validation"
name = "Test"
version = "1.0.0"
network = "none"
image = "local/test:dev"
dataset_input_contract = "biocompass.toxicity-screening.v1"
"#;
        assert!(PluginManifest::from_toml_str(wrong_contract, true).is_err());
    }

    #[test]
    fn sec_24_scaffold_validates_strict_plugin_id_escapes_templates_and_refuses_overwrite() {
        for bad_id in [
            "../evil/slug",
            "alithea/slug/extra",
            "alithea/slug'alert(1)",
            "alithea\"/slug",
            "Alithea/slug",
            "alithea/slug\nnetwork = \"all\"",
            "no-slash",
        ] {
            assert!(
                render_template_files(TemplateKind::PythonUv, bad_id, "Safe Name", true).is_err(),
                "SEC-24 must reject invalid plugin_id '{bad_id}'"
            );
        }
        assert!(
            render_template_files(
                TemplateKind::PythonUv,
                "alithea/clean-mod",
                "Bad\nnetwork = \"all\"",
                true
            )
            .is_err()
        );
        let tricky_name = r#"HLA "Binder" <script>alert(1)</script> & O'Brien"#;
        let files = render_template_files(
            TemplateKind::PythonUv,
            "alithea/clean-mod",
            tricky_name,
            true,
        )
        .expect("render with tricky display_name");
        let parsed: PluginManifest =
            toml::from_str(&files["plugin.toml"]).expect("plugin.toml must parse cleanly");
        assert_eq!(parsed.name, tricky_name);
        let html = &files["ui/dist/index.html"];
        assert!(
            !html.contains("<script>alert(1)</script>"),
            "SEC-24 must HTML-escape display_name in ui/dist/index.html"
        );
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));

        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("clean-mod");
        write_template_to_dir(
            TemplateKind::PythonUv,
            "alithea/clean-mod",
            "Clean Mod",
            false,
            &target,
        )
        .expect("first scaffold succeeds");
        let second = write_template_to_dir(
            TemplateKind::PythonUv,
            "alithea/clean-mod",
            "Overwrite Attempt",
            false,
            &target,
        )
        .expect_err("SEC-24 must refuse overwriting existing directory");
        assert!(matches!(second, CompassError::IntegrityViolation(_)));
    }

    #[test]
    fn sec_20_and_sec_30_bundled_hla_plugins_python_regression_suite() {
        let ws_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("workspace root");
        let test_script = ws_root.join("plugins/test_sec_20_30_bundled_plugins.py");
        let out = std::process::Command::new("python3")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .arg(&test_script)
            .arg("-v")
            .output()
            .expect("run bundled plugin python tests");
        assert!(
            out.status.success(),
            "bundled plugin SEC-20/SEC-30 tests failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn validates_child_modules_and_max_child_wait_sec() {
        assert_eq!(MAX_UI_BUNDLE_BYTES, 32 * 1024 * 1024);

        let valid_toml = r#"
id = "alithea/notebook"
name = "Notebook"
version = "1.0.0"
image = "local/notebook:dev"
child_modules = ["alithea/hla-motif-enrichment", "alithea/peptide-qc"]
max_child_wait_sec = 3600
"#;
        let parsed = PluginManifest::from_toml_str(valid_toml, true).expect("valid child_modules");
        assert_eq!(
            parsed.child_modules,
            ChildModules::List(vec![
                "alithea/hla-motif-enrichment".into(),
                "alithea/peptide-qc".into()
            ])
        );
        assert_eq!(parsed.max_child_wait_sec, Some(3600));
        assert!(parsed.child_modules.permits("alithea/peptide-qc"));
        assert!(!parsed.child_modules.permits("alithea/other"));
        assert_eq!(
            parsed.effective_max_child_spend_milli_act(),
            Some(DEFAULT_MAX_CHILD_SPEND_MILLI_ACT)
        );

        // `"caller"` follows the initiating principal's authority and round-trips as a string.
        let caller_toml = r#"
id = "alithea/notebook"
name = "Notebook"
version = "1.0.0"
image = "local/notebook:dev"
child_modules = "caller"
max_child_spend_milli_act = 2500
"#;
        let caller = PluginManifest::from_toml_str(caller_toml, true).expect("caller child_modules");
        assert!(caller.child_modules.is_caller());
        assert!(caller.child_modules.permits("alithea/anything"));
        assert_eq!(caller.effective_max_child_spend_milli_act(), Some(2500));
        let as_json = serde_json::to_value(&caller).unwrap();
        assert_eq!(as_json["child_modules"], json!("caller"));
        let back: PluginManifest = serde_json::from_value(as_json).unwrap();
        assert!(back.child_modules.is_caller());

        // Any other keyword, and a cap without children, are rejected.
        for bad in [
            "child_modules = \"everyone\"\n",
            "max_child_spend_milli_act = 100\n",
            "child_modules = \"caller\"\nmax_child_spend_milli_act = -1\n",
        ] {
            let doc = format!(
                "id = \"alithea/notebook\"\nname = \"N\"\nversion = \"1.0.0\"\nimage = \"local/n:dev\"\n{bad}"
            );
            assert!(PluginManifest::from_toml_str(&doc, true).is_err(), "{bad}");
        }
        // No children: no cap.
        let plain = PluginManifest::from_toml_str(
            "id = \"alithea/n\"\nname = \"N\"\nversion = \"1.0.0\"\nimage = \"local/n:dev\"\n",
            true,
        )
        .unwrap();
        assert!(plain.child_modules.is_empty());
        assert_eq!(plain.effective_max_child_spend_milli_act(), None);

        // Self-reference rejected
        let self_ref = r#"
id = "alithea/notebook"
name = "Notebook"
version = "1.0.0"
image = "local/notebook:dev"
child_modules = ["alithea/notebook"]
"#;
        assert!(PluginManifest::from_toml_str(self_ref, true).is_err());

        // Duplicate rejected
        let dup = r#"
id = "alithea/notebook"
name = "Notebook"
version = "1.0.0"
image = "local/notebook:dev"
child_modules = ["alithea/a", "alithea/a"]
"#;
        assert!(PluginManifest::from_toml_str(dup, true).is_err());

        // Bad slug rejected
        let bad_slug = r#"
id = "alithea/notebook"
name = "Notebook"
version = "1.0.0"
image = "local/notebook:dev"
child_modules = ["../evil/mod"]
"#;
        assert!(PluginManifest::from_toml_str(bad_slug, true).is_err());

        // Out-of-range max_child_wait_sec rejected
        for bad_wait in [0_u64, MAX_CHILD_WAIT_SEC + 1] {
            let doc = format!(
                "id = \"alithea/notebook\"\nname = \"N\"\nversion = \"1.0.0\"\nimage = \"local/n:dev\"\nmax_child_wait_sec = {bad_wait}\n"
            );
            assert!(PluginManifest::from_toml_str(&doc, true).is_err());
        }
    }
}
