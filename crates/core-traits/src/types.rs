//! Core domain records, error taxonomy, OCI image references, and data grants.

use crate::ids::{CatalogId, CreditAmountAct, NodeId, OrgId, PluginId, ProjectId, RunId, SessionId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Maximum wall-clock time to prepare an immutable OCI image before its signed execution budget.
pub const OCI_IMAGE_PREPARATION_TIMEOUT_SECS: u64 = 300;

/// Structured error type shared across all BioCompass domain crates.
#[derive(Debug, Error)]
pub enum CompassError {
    #[error("resource not found: {0}")]
    NotFound(String),
    #[error("validation error: {0}")]
    Validation(String),
    #[error("access denied by grant policy: {0}")]
    AccessDenied(String),
    #[error("cryptographic or SHA-256 integrity violation: {0}")]
    IntegrityViolation(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("container engine error: {0}")]
    ContainerEngine(String),
    #[error("froglet protocol error: {0}")]
    Protocol(String),
    #[error("payment or ACT ledger error: {0}")]
    Payment(String),
    #[error("rate limit exceeded: {0}")]
    RateLimited(String),
    #[error("upstream LLM provider error: {0}")]
    UpstreamLlm(String),
    #[error("internal error: {0}")]
    Internal(String),
}

/// Result alias for BioCompass operations.
pub type CompassResult<T> = Result<T, CompassError>;

impl CompassError {
    /// Canonical machine-readable error code for HTTP/API responses and logs.
    pub fn code(&self) -> &'static str {
        self.error_code().as_str()
    }

    /// Coherent, user-facing error summary with redundant lowercase prefixes removed
    /// and sentence casing applied.
    pub fn user_message(&self) -> String {
        let raw = match self {
            Self::NotFound(m)
            | Self::Validation(m)
            | Self::AccessDenied(m)
            | Self::IntegrityViolation(m)
            | Self::Storage(m)
            | Self::ContainerEngine(m)
            | Self::Protocol(m)
            | Self::Payment(m)
            | Self::RateLimited(m)
            | Self::UpstreamLlm(m)
            | Self::Internal(m) => m.trim(),
        };
        if raw.is_empty() {
            return self.to_string();
        }
        let lower = raw.to_ascii_lowercase();
        match self {
            Self::NotFound(_)
                if !lower.contains("not found")
                    && !lower.contains("not registered")
                    && !lower.contains("not installed")
                    && !lower.contains("unknown")
                    && !lower.contains("has no ")
                    && !lower.contains("does not exist") =>
            {
                format!("{} not found.", capitalize_first(raw))
            }
            Self::AccessDenied(_) if !lower.starts_with("access denied") => {
                format!("Access denied: {}", capitalize_first(raw))
            }
            Self::IntegrityViolation(_) if !lower.starts_with("integrity") => {
                format!("Integrity check failed: {}", capitalize_first(raw))
            }
            Self::Payment(_) if !lower.starts_with("credit") && !lower.starts_with("payment") => {
                format!("Credit & billing error: {}", capitalize_first(raw))
            }
            Self::RateLimited(_) if !lower.starts_with("rate limit") => {
                format!("Rate limit exceeded: {}", capitalize_first(raw))
            }
            Self::Protocol(_) if !lower.starts_with("protocol") => {
                format!("Protocol verification error: {}", capitalize_first(raw))
            }
            _ => capitalize_first(raw),
        }
    }

    /// Optional actionable next step for resolving this error.
    pub fn hint(&self) -> Option<&'static str> {
        let lower = match self {
            Self::NotFound(m)
            | Self::Validation(m)
            | Self::AccessDenied(m)
            | Self::IntegrityViolation(m)
            | Self::Storage(m)
            | Self::ContainerEngine(m)
            | Self::Protocol(m)
            | Self::Payment(m)
            | Self::RateLimited(m)
            | Self::UpstreamLlm(m)
            | Self::Internal(m) => m.to_ascii_lowercase(),
        };
        match self {
            Self::ContainerEngine(_) => {
                if lower.contains("not reachable") || lower.contains("docker") || lower.contains("podman") {
                    Some("Ensure Docker Desktop or rootless Podman is running, then retry.")
                } else if lower.contains("timeout") {
                    Some("Increase container timeout_sec in plugin.toml or narrow input size.")
                } else {
                    Some("Check container logs in the Runs tab for details.")
                }
            }
            Self::UpstreamLlm(_) => {
                if lower.contains("no llm configured") || lower.contains("no llm backend") {
                    Some("Open Settings -> Agent to select and configure a model provider.")
                } else if lower.contains("401")
                    || lower.contains("403")
                    || lower.contains("unauthorized")
                    || lower.contains("api key")
                    || lower.contains("auth")
                {
                    Some("Verify your API key or CLI sign-in under Settings -> Agent.")
                } else if lower.contains("429") || lower.contains("rate limit") {
                    Some("The provider rate limit was reached. Wait briefly or switch models.")
                } else if lower.contains("timed out") || lower.contains("timeout") {
                    Some("The model timed out. Retry or try a faster model/lower reasoning effort.")
                } else if lower.contains("not found on path") || lower.contains("no such file") {
                    Some("Install the CLI backend on PATH or choose an HTTP/other provider in Settings -> Agent.")
                } else {
                    None
                }
            }
            Self::AccessDenied(_) => {
                if matches!(
                    self.error_code(),
                    crate::error::ErrorCode::OffListModule
                        | crate::error::ErrorCode::OffListCatalog
                        | crate::error::ErrorCode::ChildModuleNotAllowed
                ) {
                    Some("Review the credential's allowed modules/catalogs or module manifest allowlist.")
                } else if matches!(
                    self.error_code(),
                    crate::error::ErrorCode::Unauthenticated
                        | crate::error::ErrorCode::AgentCredentialExpired
                        | crate::error::ErrorCode::AgentCredentialRevoked
                ) {
                    Some("Sign in or provide valid, non-expired organization or agent credentials.")
                } else if lower.contains("only available in cloud-web mode")
                    || lower.contains("hosted workspace")
                    || lower.contains("paired native workspace")
                    || lower.contains("paired, loopback native workspace")
                {
                    Some("Connect to a BioCompass Cloud hub (--mode cloud-web or paired workspace) to use hosted organization features.")
                } else if lower.contains("owner link")
                    || lower.contains("local owner")
                    || lower.contains("native workspace")
                {
                    Some("Open the local owner link from <data-dir>/owner-link.txt on this workstation.")
                } else if lower.contains("unauthenticated")
                    || lower.contains("sign in")
                    || lower.contains("invalid session")
                {
                    Some("Sign in or provide valid organization credentials.")
                } else {
                    Some("Review your active Dataset Grant policy on the Data page or adjust query filters.")
                }
            }
            Self::Payment(_) => {
                if matches!(
                    self.error_code(),
                    crate::error::ErrorCode::AgentCeilingExceeded
                ) {
                    Some("The agent credential's daily or total spending ceiling was reached. Ask an organization admin to adjust or issue a new credential.")
                } else if matches!(
                    self.error_code(),
                    crate::error::ErrorCode::InsufficientFunds
                        | crate::error::ErrorCode::DailyActBreakerExceeded
                ) || lower.contains("insufficient")
                    || lower.contains("daily act")
                {
                    Some("Check your available ACT credits and daily ACT breaker in Settings -> Usage.")
                } else {
                    None
                }
            }
            Self::RateLimited(_) => {
                if self.error_code() == crate::error::ErrorCode::RowBudgetExceeded {
                    Some("The dataset grant's daily row budget was reached. Wait for the next UTC day or request a higher budget.")
                } else {
                    Some("Wait briefly before retrying; your organization's burst request rate limit was reached.")
                }
            }
            Self::IntegrityViolation(_) => {
                match self.error_code() {
                    crate::error::ErrorCode::UiBundleTampered => {
                        Some("Refused tampered module UI bundle; retry or contact the module publisher.")
                    }
                    crate::error::ErrorCode::DocVersionConflict => {
                        Some("Reload the latest document version and retry the update with the current base_version.")
                    }
                    _ => Some("Verify image @sha256 digests and catalog SHA-256 checksums before retrying."),
                }
            }
            _ => None,
        }
    }
}

fn capitalize_first(input: &str) -> String {
    let trimmed = input.trim();
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(first) => {
            let mut out = String::with_capacity(trimmed.len());
            for ch in first.to_uppercase() {
                out.push(ch);
            }
            out.push_str(chars.as_str());
            out
        }
        None => String::new(),
    }
}

/// Validated OCI container image reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciImageRef {
    /// Full image reference string.
    pub raw: String,
    /// Repository coordinate without digest/tag (e.g. `registry.alithea.bio/alithea/hla-binding-net`).
    pub repository: String,
    /// Optional immutable SHA-256 manifest digest (`64` lowercase hex chars).
    pub sha256_digest: Option<String>,
    /// Optional mutable local development tag (permitted only when local dev tags are explicitly enabled).
    pub local_tag: Option<String>,
}

impl OciImageRef {
    /// Validate a single OCI Distribution repository path component (`[a-z0-9]+([._-][a-z0-9]+)*`),
    /// strictly rejecting `.`, `..`, empty segments, control characters, and query/fragment delimiters.
    fn is_valid_repo_segment(seg: &str) -> bool {
        if seg.is_empty() || seg == "." || seg == ".." || seg.len() > 128 {
            return false;
        }
        let bytes = seg.as_bytes();
        let is_alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
        if !is_alnum(bytes[0]) || !is_alnum(bytes[bytes.len() - 1]) {
            return false;
        }
        bytes.iter().all(|&b| is_alnum(b) || matches!(b, b'.' | b'_' | b'-'))
    }

    /// Validate an OCI registry authority (`host[:port]`, supporting DNS names, IPv4, and `[IPv6]`)
    /// with zero userinfo (`@`), path, query, fragment, whitespace, or control characters (`SEC-23`).
    fn is_valid_registry_host(authority: &str) -> bool {
        if authority.is_empty() || authority.len() > 255 {
            return false;
        }
        if authority
            .bytes()
            .any(|b| b < 0x21 || b == 0x7f || matches!(b, b'@' | b'?' | b'#' | b'/' | b'\\' | b'%'))
        {
            return false;
        }
        if let Some(rest) = authority.strip_prefix('[') {
            let Some((ip_part, after_bracket)) = rest.split_once(']') else {
                return false;
            };
            if ip_part.parse::<std::net::Ipv6Addr>().is_err() {
                return false;
            }
            if after_bracket.is_empty() {
                return true;
            }
            let Some(port_str) = after_bracket.strip_prefix(':') else {
                return false;
            };
            return port_str.parse::<u16>().is_ok_and(|p| p > 0);
        }
        let (host, port_opt) = match authority.split_once(':') {
            Some((h, p)) => {
                if p.contains(':') {
                    return false;
                }
                (h, Some(p))
            }
            None => (authority, None),
        };
        if let Some(port_str) = port_opt
            && !port_str.parse::<u16>().is_ok_and(|p| p > 0)
        {
            return false;
        }
        if host.is_empty() || host == "." || host == ".." || host.contains("..") {
            return false;
        }
        if host.parse::<std::net::Ipv4Addr>().is_ok() {
            return true;
        }
        host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
    }

    /// Validate the repository portion (`[registry_host/]org/image`) per OCI Distribution Spec (`SEC-23`).
    fn validate_repository(repo: &str) -> CompassResult<()> {
        if repo.is_empty()
            || repo.len() > 512
            || repo.chars().any(|c| c.is_control() || c.is_whitespace())
            || repo.contains('?')
            || repo.contains('#')
            || repo.contains('\\')
            || repo.contains('%')
            || repo.contains('@')
            || repo.contains("://")
        {
            return Err(CompassError::Validation(format!(
                "invalid OCI image repository '{repo}': disallowed characters or syntax"
            )));
        }
        let segments: Vec<&str> = repo.split('/').collect();
        if segments.len() < 2 || segments.iter().any(|s| s.is_empty()) {
            return Err(CompassError::Validation(format!(
                "invalid OCI image repository '{repo}': expected non-empty '/'-separated segments"
            )));
        }
        let first = segments[0];
        let first_is_registry = first.contains('.') || first.contains(':') || first.eq_ignore_ascii_case("localhost");
        let path_segments = if first_is_registry {
            if !Self::is_valid_registry_host(first) {
                return Err(CompassError::Validation(format!(
                    "invalid OCI registry host '{first}' in '{repo}'"
                )));
            }
            &segments[1..]
        } else {
            &segments[..]
        };
        if path_segments.is_empty() || !path_segments.iter().all(|seg| Self::is_valid_repo_segment(seg)) {
            return Err(CompassError::Validation(format!(
                "invalid OCI repository path segment in '{repo}'"
            )));
        }
        Ok(())
    }

    fn is_valid_local_tag(tag: &str) -> bool {
        !tag.is_empty()
            && tag.len() <= 128
            && !tag.starts_with('.')
            && !tag.starts_with('-')
            && tag
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    }

    /// Parse an OCI image reference, requiring `@sha256:<64-hex>` unless `allow_local_tag` is `true`
    /// and the image uses the `local/` namespace.
    pub fn parse(raw: &str, allow_local_tag: bool) -> CompassResult<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(CompassError::Validation("OCI image reference cannot be empty".into()));
        }
        if trimmed.chars().any(|c| c.is_control() || c.is_whitespace())
            || trimmed.contains('?')
            || trimmed.contains('#')
            || trimmed.contains('\\')
        {
            return Err(CompassError::Validation(format!(
                "OCI image reference '{trimmed}' contains forbidden characters"
            )));
        }
        if let Some((repo, digest_part)) = trimmed.split_once("@sha256:") {
            if repo.is_empty() {
                return Err(CompassError::Validation(
                    "OCI image repository before @sha256: cannot be empty".into(),
                ));
            }
            Self::validate_repository(repo)?;
            let is_valid_hex = digest_part.len() == 64
                && digest_part
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if !is_valid_hex {
                return Err(CompassError::Validation(format!(
                    "OCI image digest must be 64 lowercase hex chars, got '{digest_part}'"
                )));
            }
            return Ok(Self {
                raw: trimmed.to_owned(),
                repository: repo.to_owned(),
                sha256_digest: Some(digest_part.to_owned()),
                local_tag: None,
            });
        }

        if allow_local_tag && trimmed.starts_with("local/") {
            if trimmed.contains('@') {
                return Err(CompassError::Validation(format!(
                    "local OCI image '{trimmed}' cannot contain '@'"
                )));
            }
            let (repo, tag) = match trimmed.split_once(':') {
                Some((r, t)) if !t.contains(':') => (r, t),
                Some(_) => {
                    return Err(CompassError::Validation(format!(
                        "local OCI image '{trimmed}' has malformed tag separator"
                    )));
                }
                None => (trimmed, "dev"),
            };
            Self::validate_repository(repo)?;
            if !Self::is_valid_local_tag(tag) {
                return Err(CompassError::Validation(format!(
                    "local OCI image tag '{tag}' is invalid"
                )));
            }
            return Ok(Self {
                raw: trimmed.to_owned(),
                repository: repo.to_owned(),
                sha256_digest: None,
                local_tag: Some(tag.to_owned()),
            });
        }

        Err(CompassError::Validation(format!(
            "OCI image '{trimmed}' must be pinned by immutable '@sha256:<64-hex>' digest"
        )))
    }

    /// Return the 64-char hex digest used in Froglet receipts (or deterministic SHA-256 of local dev ref).
    pub fn canonical_module_hash(&self) -> String {
        if let Some(digest) = &self.sha256_digest {
            digest.clone()
        } else {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(self.raw.as_bytes()))
        }
    }
}

/// Schema-generic column descriptor for a catalog table (§5.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnDescriptor {
    pub name: String,
    pub data_type: String,
    #[serde(default)]
    pub nullable: bool,
}

/// Validated table schema inside a sealed catalog snapshot (§5.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<ColumnDescriptor>,
    #[serde(default)]
    pub primary_key: Vec<String>,
    #[serde(default)]
    pub row_count: u64,
}

/// Scalar literal bound via `?` parameters in `FilterExpr` (never interpolated into raw SQL).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScalarValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

impl From<&str> for ScalarValue {
    fn from(v: &str) -> Self {
        Self::Text(v.to_owned())
    }
}

impl From<String> for ScalarValue {
    fn from(v: String) -> Self {
        Self::Text(v)
    }
}

impl From<i64> for ScalarValue {
    fn from(v: i64) -> Self {
        Self::Int(v)
    }
}

impl From<i32> for ScalarValue {
    fn from(v: i32) -> Self {
        Self::Int(i64::from(v))
    }
}

impl From<f64> for ScalarValue {
    fn from(v: f64) -> Self {
        Self::Float(v)
    }
}

impl From<bool> for ScalarValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

/// Recursive, schema-validated predicate AST compiled exclusively to `?`-bound SQL (§5.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FilterExpr {
    And { exprs: Vec<FilterExpr> },
    Or { exprs: Vec<FilterExpr> },
    Eq { column: String, value: ScalarValue },
    Neq { column: String, value: ScalarValue },
    In { column: String, values: Vec<ScalarValue> },
    Lt { column: String, value: ScalarValue },
    Lte { column: String, value: ScalarValue },
    Gt { column: String, value: ScalarValue },
    Gte { column: String, value: ScalarValue },
    IsNull { column: String },
    IsNotNull { column: String },
    StartsWith { column: String, prefix: String },
}

impl FilterExpr {
    pub fn eq(column: impl Into<String>, value: impl Into<ScalarValue>) -> Self {
        Self::Eq {
            column: column.into(),
            value: value.into(),
        }
    }

    pub fn neq(column: impl Into<String>, value: impl Into<ScalarValue>) -> Self {
        Self::Neq {
            column: column.into(),
            value: value.into(),
        }
    }

    pub fn in_values<I, V>(column: impl Into<String>, values: I) -> Self
    where
        I: IntoIterator<Item = V>,
        V: Into<ScalarValue>,
    {
        Self::In {
            column: column.into(),
            values: values.into_iter().map(Into::into).collect(),
        }
    }

    pub fn lt(column: impl Into<String>, value: impl Into<ScalarValue>) -> Self {
        Self::Lt {
            column: column.into(),
            value: value.into(),
        }
    }

    pub fn lte(column: impl Into<String>, value: impl Into<ScalarValue>) -> Self {
        Self::Lte {
            column: column.into(),
            value: value.into(),
        }
    }

    pub fn gt(column: impl Into<String>, value: impl Into<ScalarValue>) -> Self {
        Self::Gt {
            column: column.into(),
            value: value.into(),
        }
    }

    pub fn gte(column: impl Into<String>, value: impl Into<ScalarValue>) -> Self {
        Self::Gte {
            column: column.into(),
            value: value.into(),
        }
    }

    pub fn starts_with(column: impl Into<String>, prefix: impl Into<String>) -> Self {
        Self::StartsWith {
            column: column.into(),
            prefix: prefix.into(),
        }
    }

    /// Combine two optional predicates with canonical flattened/deduplicated `AND` semantics
    /// so repeated policy intersections (`intersect_grants`) are strictly idempotent.
    pub fn combine_and(left: Option<Self>, right: Option<Self>) -> Option<Self> {
        let mut terms = Vec::new();
        fn push_flattened(expr: FilterExpr, out: &mut Vec<FilterExpr>) {
            match expr {
                FilterExpr::And { exprs } => {
                    for sub in exprs {
                        push_flattened(sub, out);
                    }
                }
                other => {
                    if !out.contains(&other) {
                        out.push(other);
                    }
                }
            }
        }
        if let Some(l) = left {
            push_flattened(l, &mut terms);
        }
        if let Some(r) = right {
            push_flattened(r, &mut terms);
        }
        match terms.len() {
            0 => None,
            1 => terms.into_iter().next(),
            _ => Some(Self::And { exprs: terms }),
        }
    }
}

/// Sort direction for `QueryAst` `order_by` clauses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    #[default]
    Asc,
    Desc,
}

/// Validated sort column specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortSpec {
    pub column: String,
    #[serde(default)]
    pub direction: SortDirection,
}

/// Additional same-plane catalog attached at a pinned sealed snapshot (§5.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachedCatalogPin {
    pub catalog_id: CatalogId,
    pub grant_id: String,
    #[serde(default)]
    pub snapshot_id: Option<u64>,
}

/// Schema-generic, SQL-free query AST over one or more pinned same-plane catalogs (§5.2).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QueryAst {
    /// Target table name (`None` selects the catalog's primary table).
    #[serde(default)]
    pub table: Option<String>,
    /// Projected column names (`empty` selects all columns in schema order).
    #[serde(default)]
    pub columns: Vec<String>,
    /// Optional recursive bound predicate tree.
    #[serde(default)]
    pub filter: Option<FilterExpr>,
    /// Optional sort specifications validated against schema columns.
    #[serde(default)]
    pub order_by: Vec<SortSpec>,
    /// Maximum rows to return (clamped by `grant.max_rows_per_query`).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Zero-based row offset.
    #[serde(default)]
    pub offset: Option<usize>,
    /// Optional pinned sealed snapshot ID (`None` selects latest sealed snapshot).
    #[serde(default)]
    pub snapshot_id: Option<u64>,
    /// Optional additional same-plane catalogs unioned by schema-compatible table.
    #[serde(default)]
    pub additional_sources: Vec<AttachedCatalogPin>,
}

/// Non-zero catalog access level (`none` = no `DatasetGrant` exists; `compute_only < rows`, §2.1 / §5.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantLevel {
    #[default]
    ComputeOnly,
    Rows,
}

impl GrantLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ComputeOnly => "compute_only",
            Self::Rows => "rows",
        }
    }
}

/// A reviewed module selects a numeric measure and, optionally, a publisher-approved membership path.
/// The original string form remains the wire representation for an unfiltered measure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ScopedQueryDefinition {
    Sum(String),
    WhereExists { measure: String, where_exists: String },
}

impl ScopedQueryDefinition {
    pub fn measure(&self) -> &str {
        match self {
            Self::Sum(measure) | Self::WhereExists { measure, .. } => measure,
        }
    }

    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Sum(_) => None,
            Self::WhereExists { where_exists, .. } => Some(where_exists),
        }
    }
}

/// Three-level scientific dataset access grant (`compute_only` vs `rows`, zero `***` column masking, §5.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetGrant {
    pub grant_id: String,
    pub label: String,
    pub org_id: OrgId,
    pub catalog_id: CatalogId,
    pub level: GrantLevel,
    /// Optional row-level predicate enforced before user queries or custom module staging.
    #[serde(default)]
    pub row_scope: Option<FilterExpr>,
    /// Optional daily row budget across `QueryCatalog` / custom staging (`None` = unlimited).
    #[serde(default)]
    pub daily_row_budget: Option<u64>,
    /// Hard ceiling on rows returned per query or staged into a custom module (`0` when `compute_only`).
    pub max_rows_per_query: usize,
    /// Whether slices from this catalog may be exported to local laptop runs or downloaded locally.
    #[serde(default)]
    pub allow_local_export: bool,
}

impl DatasetGrant {
    /// Full `rows` access for local single-tenant scientist mode or catalog owners.
    pub fn full_access(org_id: OrgId, catalog_id: CatalogId) -> Self {
        Self {
            grant_id: format!("grant-full-{}", catalog_id.as_str()),
            label: "Full Research Access (Unmasked Rows · Local Export Enabled)".into(),
            org_id,
            catalog_id,
            level: GrantLevel::Rows,
            row_scope: None,
            daily_row_budget: None,
            max_rows_per_query: 100_000,
            allow_local_export: true,
        }
    }

    /// Scoped `rows` partner grant (`organ = 'Lung'` & NSCLC/Adjacent cohorts, daily row budget, no local export).
    pub fn scoped_rows_partner(org_id: OrgId, catalog_id: CatalogId) -> Self {
        Self {
            grant_id: format!("grant-partner-lung-rows-{}", catalog_id.as_str()),
            label: "Partner Scoped Rows (Lung Cohort · 5,000 Rows/Day · Cloud Only)".into(),
            org_id,
            catalog_id,
            level: GrantLevel::Rows,
            row_scope: Some(FilterExpr::And {
                exprs: vec![
                    FilterExpr::eq("organ", "Lung"),
                    FilterExpr::in_values("condition", ["NSCLC_Tumor", "Healthy_Adjacent"]),
                ],
            }),
            daily_row_budget: Some(5_000),
            max_rows_per_query: 1_000,
            allow_local_export: false,
        }
    }

    /// `compute_only` grant: org sees catalog card/facets and runs curated modules, with zero row queries.
    pub fn compute_only_partner(org_id: OrgId, catalog_id: CatalogId) -> Self {
        Self {
            grant_id: format!("grant-compute-only-{}", catalog_id.as_str()),
            label: "Partner Compute-Only Access (Curated Modules Only · Zero Row Queries)".into(),
            org_id,
            catalog_id,
            level: GrantLevel::ComputeOnly,
            row_scope: None,
            daily_row_budget: None,
            max_rows_per_query: 0,
            allow_local_export: false,
        }
    }

    /// Concise human-readable summary included on `RunQuote` consent cards.
    pub fn summary_line(&self) -> String {
        let budget = match self.daily_row_budget {
            Some(b) => format!("{b} rows/day"),
            None => "unlimited/day".into(),
        };
        format!(
            "{} (catalog={}, level={}, max_rows_per_query={}, budget={budget}, local_export={})",
            self.label,
            self.catalog_id,
            self.level.as_str(),
            self.max_rows_per_query,
            self.allow_local_export
        )
    }

    /// Deterministic SHA-256 digest of `(level, row_scope)` used to detect access-scope changes on Saved Views (§7.1).
    pub fn scope_digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let canonical = serde_json::json!({
            "catalog_id": self.catalog_id.as_str(),
            "level": self.level.as_str(),
            "row_scope": FilterExpr::combine_and(self.row_scope.clone(), None),
        });
        let encoded = serde_json::to_vec(&canonical).unwrap_or_default();
        hex::encode(Sha256::digest(&encoded))
    }
}

/// Durable workspace project folder grouping chat sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub project_id: ProjectId,
    pub org_id: OrgId,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Durable agent conversation session summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: SessionId,
    pub org_id: OrgId,
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    pub title: String,
    pub active_grant_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Discriminant for durable session log events (`Model-visible <=> Logged`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventKind {
    UserMessage,
    AssistantMessage,
    ToolCall,
    ToolResult,
    RunPrepared,
    RunCompleted,
    SystemNotice,
}

/// Single immutable event appended to a durable `SessionRecord`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub event_id: String,
    pub session_id: SessionId,
    pub seq: i64,
    pub kind: SessionEventKind,
    pub content: String,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
}

/// Lifecycle status of a plugin execution run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Prepared,
    Running,
    Succeeded,
    Failed,
    Canceled,
}

/// Maximum number of upstream run artifact inputs permitted in one chained run (`W6`).
pub const MAX_RUN_INPUTS: usize = 32;

/// Return `true` when `digest` is a canonical 64-character lowercase hexadecimal SHA-256 string.
pub fn is_sha256_hex(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Cryptographically bound upstream run artifact input staged under `/in/artifacts/<alias>/<name>` (`W6`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RunInput {
    pub alias: String,
    pub source_run_id: String,
    pub name: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub source_receipt_hash: String,
}

impl RunInput {
    /// Validate path safety, identifier constraints, and 64-hex SHA-256 digests.
    pub fn validate(&self) -> CompassResult<()> {
        if self.alias.is_empty()
            || self.alias.len() > 64
            || self.alias.trim() != self.alias
            || matches!(self.alias.as_str(), "." | "..")
            || !self
                .alias
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(CompassError::Validation(format!(
                "invalid run input alias '{}': expected 1..=64 ASCII [a-zA-Z0-9._-] chars",
                self.alias
            )));
        }
        if self.source_run_id.is_empty()
            || self.source_run_id.len() > 128
            || self.source_run_id.trim() != self.source_run_id
            || self.source_run_id.contains(['/', '\\', '\0'])
            || self
                .source_run_id
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(CompassError::Validation(format!(
                "invalid run input source_run_id '{}'",
                self.source_run_id
            )));
        }
        if self.name.is_empty()
            || self.name.len() > 256
            || self.name.trim() != self.name
            || self.name.contains(['\\', '\0'])
            || self.name.chars().any(char::is_control)
            || self
                .name
                .split('/')
                .any(|segment| matches!(segment, "" | "." | ".."))
        {
            return Err(CompassError::Validation(format!(
                "invalid run input artifact name '{}'",
                self.name
            )));
        }
        if !is_sha256_hex(&self.sha256) {
            return Err(CompassError::Validation(format!(
                "invalid run input sha256 '{}': expected 64 lowercase hex chars",
                self.sha256
            )));
        }
        if !is_sha256_hex(&self.source_receipt_hash) {
            return Err(CompassError::Validation(format!(
                "invalid run input source_receipt_hash '{}': expected 64 lowercase hex chars",
                self.source_receipt_hash
            )));
        }
        Ok(())
    }

    /// Relative staging path under `/in/artifacts/` (`<alias>/<name>`).
    pub fn stage_relative_path(&self) -> String {
        format!("{}/{}", self.alias, self.name)
    }
}

/// Validate a slice of [`RunInput`] entries, enforce [`MAX_RUN_INPUTS`], reject duplicate or
/// path-colliding `<alias>/<name>` stage paths, and return a deterministically sorted `Vec`.
pub fn canonicalize_run_inputs(inputs: &[RunInput]) -> CompassResult<Vec<RunInput>> {
    if inputs.len() > MAX_RUN_INPUTS {
        return Err(CompassError::Validation(format!(
            "too many run inputs ({}); maximum is {MAX_RUN_INPUTS}",
            inputs.len()
        )));
    }
    let mut staged_paths = std::collections::BTreeSet::new();
    for input in inputs {
        input.validate()?;
        let rel = input.stage_relative_path();
        if !staged_paths.insert(rel.clone()) {
            return Err(CompassError::Validation(format!(
                "duplicate run input stage path '/in/artifacts/{rel}'"
            )));
        }
    }
    for path in &staged_paths {
        for (idx, _) in path.match_indices('/') {
            if staged_paths.contains(&path[..idx]) {
                return Err(CompassError::Validation(format!(
                    "conflicting run input stage path '/in/artifacts/{path}'"
                )));
            }
        }
    }
    let mut canonical = inputs.to_vec();
    canonical.sort();
    Ok(canonical)
}

/// Request to prepare and quote a plugin container execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareRunRequest {
    pub org_id: OrgId,
    pub session_id: Option<SessionId>,
    pub plugin_id: PluginId,
    /// An exact published version. Omission keeps the installation's default selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_version: Option<String>,
    #[serde(default)]
    pub offer_id: Option<String>,
    pub params: Value,
    pub dataset_grant: Option<DatasetGrant>,
    /// Upstream run artifact inputs staged into `/in/artifacts/<alias>/<name>` (`W6`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<RunInput>,
    /// Parent run ID when spawned as a child module run (`W7`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    /// Admin-issued agent credential ID stamped at prepare time (`H1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_credential_id: Option<String>,
    /// MCP / agent tool-call ID for audit correlation (`H1`, `H2`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_tool_call_id: Option<String>,
    /// Stored module document ID whose versioned params are loaded server-side (`H5`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    /// Stored module document version paired with `doc_id` (`H5`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_version: Option<i64>,
}

/// Reject an invalid exact selector before looking up a plugin or waking a worker.
/// Version identifiers are opaque and must match the published spelling.
pub fn validate_requested_plugin_version(version: Option<&str>) -> CompassResult<()> {
    if let Some(value) = version
        && (value.is_empty() || value.trim() != value)
    {
        return Err(CompassError::Validation(
            "plugin_version must be nonempty and have no surrounding whitespace".into(),
        ));
    }
    Ok(())
}

fn default_offer_id() -> String {
    "default".into()
}

fn default_offer_name() -> String {
    "Standard Run".into()
}

/// Pre-execution quote returned before user consent (`Prepare -> Confirm` two-step gate).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunQuote {
    pub run_id: RunId,
    pub plugin_id: PluginId,
    pub plugin_version: String,
    pub image_ref: String,
    pub target_node_id: NodeId,
    #[serde(default = "default_offer_id")]
    pub offer_id: String,
    #[serde(default = "default_offer_name")]
    pub offer_name: String,
    #[serde(default)]
    pub base_fee_act: CreditAmountAct,
    pub estimated_cost_act: CreditAmountAct,
    #[serde(default)]
    pub max_input_bytes: u64,
    #[serde(default)]
    pub max_output_bytes: u64,
    #[serde(default)]
    pub max_runtime_sec: u64,
    #[serde(default)]
    pub max_memory_mb: u64,
    pub workload_hash: String,
    pub dataset_grant_summary: Option<String>,
    pub froglet_quote: Value,
    pub expires_at: DateTime<Utc>,
}

impl RunQuote {
    /// Human-readable notice when local clean-clone execution built a bundled `Dockerfile`
    /// under `local/bundled-<slug>:sha256-<digest>` because the pinned registry was unreachable (R11).
    pub fn local_bundled_fallback_notice(&self) -> Option<String> {
        let rest = self.image_ref.strip_prefix("local/bundled-")?;
        let (slug, _) = rest.split_once(':')?;
        Some(format!(
            "Pinned registry was unreachable; built bundled plugins/{slug}/Dockerfile locally ({}) under local dev allowance.",
            self.image_ref
        ))
    }
    /// Check the returned selection before offering a quote for approval or saving it for a workspace.
    pub fn validate_selection(
        &self,
        plugin_id: &PluginId,
        plugin_version: Option<&str>,
        offer_id: Option<&str>,
    ) -> CompassResult<()> {
        let offer_id = offer_id.map(str::trim).filter(|value| !value.is_empty());
        if &self.plugin_id != plugin_id
            || plugin_version.is_some_and(|version| version != self.plugin_version)
            || offer_id.is_some_and(|offer| offer != self.offer_id)
        {
            return Err(CompassError::IntegrityViolation(format!(
                "quote returned module '{}' version '{}' offer '{}' outside requested selection for '{}' (version {:?}, offer {:?})",
                self.plugin_id, self.plugin_version, self.offer_id, plugin_id, plugin_version, offer_id
            )));
        }
        Ok(())
    }
}

/// Admission of a user-approved hosted run before the worker signs a Deal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunAdmission {
    pub state: String,
    pub requested_at: DateTime<Utc>,
}

/// Persisted execution record including outputs and signed Froglet v1 artifacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: RunId,
    pub org_id: OrgId,
    pub session_id: Option<SessionId>,
    pub plugin_id: PluginId,
    pub plugin_version: String,
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission: Option<RunAdmission>,
    pub params: Value,
    pub grant: Option<DatasetGrant>,
    /// Canonical upstream run artifact inputs bound into workload_hash & Quote/Deal `extension_refs` (`W6`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<RunInput>,
    /// Parent run ID when spawned as a child run (`W7`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    /// Immutable agent credential ID stamped at prepare time (`H1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_credential_id: Option<String>,
    /// Optional MCP / agent tool-call ID for audit correlation (`H1`, `H2`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_tool_call_id: Option<String>,
    /// Module fee component in milli-ACT / milli-EUR (`H1`, `H3`, `H4`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_fee_milli_act: Option<i64>,
    /// Dataset fee component in milli-ACT / milli-EUR (`H1`, `H3`, `H4`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset_fee_milli_act: Option<i64>,
    pub quote: RunQuote,
    pub result_json: Option<Value>,
    pub artifacts: Vec<ArtifactRef>,
    /// Signed `froglet/v1` `Receipt` JSON verified offline via `froglet-verify`.
    pub froglet_receipt: Option<Value>,
    /// Full 5-artifact `froglet/v1` chain (`Descriptor`, `Offer`, `Quote`, `Deal`, `Receipt`).
    pub froglet_chain: Vec<Value>,
    pub receipt_verified: bool,
    pub logs: Vec<String>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl RunRecord {
    /// The server has accepted confirmation and the run can still advance.
    pub fn confirmation_pending(&self) -> bool {
        self.status == RunStatus::Running
            || (self.status == RunStatus::Prepared
                && self.admission.as_ref().is_some_and(|approval| approval.state == "at_capacity"))
    }
}

/// Output file artifact produced in `/out/artifacts/*` and stored in `BlobStore`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub name: String,
    pub blob_key: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub sha256: String,
}

/// Streaming container or stage lifecycle line emitted over SSE.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunLogEvent {
    pub run_id: RunId,
    pub timestamp: DateTime<Utc>,
    pub stream: String,
    pub line: String,
}

/// Double-entry ACT credit ledger transaction kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerTxKind {
    GenesisGrant,
    InvoiceTopUp,
    WebhookTopUp,
    RunReserve,
    RunSettle,
    RunRefund,
}

/// Immutable ACT ledger entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerTx {
    pub tx_id: String,
    pub org_id: OrgId,
    pub run_id: Option<RunId>,
    pub kind: LedgerTxKind,
    pub delta_act: CreditAmountAct,
    pub balance_after_act: CreditAmountAct,
    #[serde(default)]
    pub receipt_hash: Option<String>,
    pub reference: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationState {
    Reserved,
    Settled,
    Refunded,
}

/// Durable confirmation terms for one organization's original run attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReservation {
    pub idempotency_key: String,
    pub amount: CreditAmountAct,
    pub state: ReservationState,
}

/// Execution/storage plane of a reproducible Saved View (§7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedViewPlane {
    Local,
    Cloud,
}

impl SavedViewPlane {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cloud => "cloud",
        }
    }
}

/// On-demand verification status of a Saved View (§7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedViewStatus {
    Verified,
    AccessChanged,
    DataChanged,
}

impl SavedViewStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::AccessChanged => "access_changed",
            Self::DataChanged => "data_changed",
        }
    }

    pub fn parse(raw: &str) -> CompassResult<Self> {
        match raw.trim() {
            "verified" => Ok(Self::Verified),
            "access_changed" => Ok(Self::AccessChanged),
            "data_changed" => Ok(Self::DataChanged),
            other => Err(CompassError::Validation(format!(
                "invalid saved view status '{other}'"
            ))),
        }
    }
}

/// Pinned catalog snapshot & grant scope recorded inside a `SavedViewRecord` (§7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedViewSourcePin {
    pub catalog_id: CatalogId,
    pub table_name: String,
    pub snapshot_id: u64,
    pub dataset_digest: String,
    pub grant_id: String,
    pub grant_scope_digest: String,
}

/// Reproducible Saved View binding a typed `QueryAst`, pinned snapshot(s), and PK `selection_digest` (§7.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedViewRecord {
    pub view_id: String,
    pub org_id: OrgId,
    pub created_by: String,
    pub name: String,
    pub plane: SavedViewPlane,
    pub primary_source: SavedViewSourcePin,
    #[serde(default)]
    pub additional_sources: Vec<SavedViewSourcePin>,
    pub primary_key_columns: Vec<String>,
    pub query_ast: QueryAst,
    pub row_count: u64,
    pub selection_digest: String,
    #[serde(default)]
    pub last_status: Option<SavedViewStatus>,
    #[serde(default)]
    pub last_verified_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_detail: Option<String>,
    /// True on the local plane — UI badge explicitly states verification ran on the user's own machine (§7.1).
    #[serde(default)]
    pub local_provenance: bool,
    pub created_at: DateTime<Utc>,
}

/// Result of verifying / reloading a Saved View (`verified | access_changed | data_changed`, §7.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedViewVerificationReport {
    pub view_id: String,
    pub status: SavedViewStatus,
    pub cached: bool,
    pub local_provenance: bool,
    pub provenance_label: String,
    pub saved_row_count: u64,
    pub saved_selection_digest: String,
    #[serde(default)]
    pub current_row_count: Option<u64>,
    #[serde(default)]
    pub current_selection_digest: Option<String>,
    pub detail: String,
    pub verified_at: DateTime<Utc>,
}

/// Tabular file format accepted by `biocompass catalog import` and the Quarantine Import Wizard (§7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestFormat {
    Parquet,
    Csv,
    Tsv,
}

impl IngestFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Parquet => "parquet",
            Self::Csv => "csv",
            Self::Tsv => "tsv",
        }
    }

    pub fn parse(raw: &str) -> CompassResult<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "parquet" | "pq" => Ok(Self::Parquet),
            "csv" => Ok(Self::Csv),
            "tsv" | "tab" => Ok(Self::Tsv),
            other => Err(CompassError::Validation(format!(
                "unsupported ingest format '{other}'; expected parquet, csv, or tsv"
            ))),
        }
    }

    pub fn from_filename(filename: &str) -> CompassResult<Self> {
        let lower = filename.trim().to_ascii_lowercase();
        if lower.ends_with(".parquet") || lower.ends_with(".pq") {
            Ok(Self::Parquet)
        } else if lower.ends_with(".csv") {
            Ok(Self::Csv)
        } else if lower.ends_with(".tsv") || lower.ends_with(".tab") {
            Ok(Self::Tsv)
        } else {
            Err(CompassError::Validation(format!(
                "cannot infer ingest format from '{filename}'; specify --format parquet|csv|tsv"
            )))
        }
    }
}

/// Commit semantics for catalog table import (`append | replace | edit`, §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestMode {
    Append,
    Replace,
    Edit,
}

impl IngestMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Append => "append",
            Self::Replace => "replace",
            Self::Edit => "edit",
        }
    }

    pub fn parse(raw: &str) -> CompassResult<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "append" => Ok(Self::Append),
            "replace" => Ok(Self::Replace),
            "edit" | "upsert" => Ok(Self::Edit),
            other => Err(CompassError::Validation(format!(
                "unsupported ingest mode '{other}'; expected append, replace, or edit"
            ))),
        }
    }
}

/// Hosting plane for a published Froglet Dataset Offer (§7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetHostingMode {
    CloudHosted,
    NodeHosted,
}

impl DatasetHostingMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CloudHosted => "cloud_hosted",
            Self::NodeHosted => "node_hosted",
        }
    }

    pub fn parse(raw: &str) -> CompassResult<Self> {
        match raw.trim() {
            "cloud_hosted" => Ok(Self::CloudHosted),
            "node_hosted" => Ok(Self::NodeHosted),
            other => Err(CompassError::Validation(format!(
                "invalid dataset hosting mode '{other}'; expected cloud_hosted or node_hosted"
            ))),
        }
    }
}

/// Discovery audience for a signed dataset offer. Legacy listings retain their
/// original authenticated visibility during migration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetOfferScope {
    #[default]
    Private,
    CustomShared,
    Global,
    LegacySessionOnly,
}

/// Published Froglet Dataset Offer (`cloud_hosted` or `node_hosted`, §7.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetOfferRecord {
    pub offer_id: String,
    pub publisher_org_id: OrgId,
    #[serde(default)]
    pub scope: DatasetOfferScope,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_orgs: Vec<OrgId>,
    #[serde(default)]
    pub approval_required: bool,
    pub catalog_id: CatalogId,
    pub hosting_mode: DatasetHostingMode,
    #[serde(default)]
    pub node_id: Option<NodeId>,
    pub title: String,
    pub description: String,
    pub default_level: GrantLevel,
    #[serde(default)]
    pub row_scope: Option<FilterExpr>,
    #[serde(default)]
    pub daily_row_budget: Option<u64>,
    pub max_rows_per_query: usize,
    pub price_milli_act: u64,
    pub provider_pubkey: String,
    pub descriptor_doc: Value,
    pub froglet_offer_doc: Value,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

/// Provenance / origin of a listed module (`U2`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleSource {
    /// Authored or installed directly inside the active workspace.
    #[default]
    Workspace,
    /// Published in the caller's own organization cloud hub.
    OrgCloud,
    /// Offered by an external network provider hub or peer.
    NetworkProvider,
}

impl ModuleSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::OrgCloud => "org_cloud",
            Self::NetworkProvider => "network_provider",
        }
    }
}

/// Execution plane where a listed module runs (`U2`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleRunsIn {
    /// Runs on the local workstation / laptop container engine.
    #[default]
    Local,
    /// Runs on the connected cloud hub's managed worker pool.
    Cloud,
    /// Runs on an organization-owned `provider-edge` worker node.
    ProviderEdge,
    /// Runs remotely at an external network provider.
    NetworkProvider,
}

impl ModuleRunsIn {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cloud => "cloud",
            Self::ProviderEdge => "provider_edge",
            Self::NetworkProvider => "network_provider",
        }
    }
}

/// Shared module listing metadata (`U1`, `U2`) used across hub, harness server, and web DTOs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleListingMeta {
    #[serde(default)]
    pub source: ModuleSource,
    #[serde(default)]
    pub runs_in: ModuleRunsIn,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_bundle_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_bundle_size_bytes: Option<u64>,
    #[serde(default)]
    pub can_install: bool,
    #[serde(default)]
    pub installed_locally: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_label: Option<String>,
}
