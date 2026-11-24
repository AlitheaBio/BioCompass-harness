//! Stable machine-readable error codes shared across BioCompass harness, hub, MCP, and UI.

use crate::types::CompassError;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Stable error code string constants for wire protocols, MCP, logs, and tests.
pub const ERR_NOT_FOUND: &str = "NOT_FOUND";
pub const ERR_VALIDATION_ERROR: &str = "VALIDATION_ERROR";
pub const ERR_ACCESS_DENIED: &str = "ACCESS_DENIED";
pub const ERR_INTEGRITY_VIOLATION: &str = "INTEGRITY_VIOLATION";
pub const ERR_STORAGE_ERROR: &str = "STORAGE_ERROR";
pub const ERR_CONTAINER_ENGINE_ERROR: &str = "CONTAINER_ENGINE_ERROR";
pub const ERR_PROTOCOL_ERROR: &str = "PROTOCOL_ERROR";
pub const ERR_PAYMENT_ERROR: &str = "PAYMENT_ERROR";
pub const ERR_RATE_LIMITED: &str = "RATE_LIMITED";
pub const ERR_UPSTREAM_LLM_ERROR: &str = "UPSTREAM_LLM_ERROR";
pub const ERR_INTERNAL_ERROR: &str = "INTERNAL_ERROR";

pub const ERR_INSUFFICIENT_FUNDS: &str = "INSUFFICIENT_FUNDS";
pub const ERR_DAILY_ACT_BREAKER_EXCEEDED: &str = "DAILY_ACT_BREAKER_EXCEEDED";
pub const ERR_AGENT_CEILING_EXCEEDED: &str = "AGENT_CEILING_EXCEEDED";
pub const ERR_AGENT_CREDENTIAL_REVOKED: &str = "AGENT_CREDENTIAL_REVOKED";
pub const ERR_AGENT_CREDENTIAL_EXPIRED: &str = "AGENT_CREDENTIAL_EXPIRED";
pub const ERR_IDEMPOTENCY_CONFLICT: &str = "IDEMPOTENCY_CONFLICT";
pub const ERR_OFF_LIST_MODULE: &str = "OFF_LIST_MODULE";
pub const ERR_OFF_LIST_CATALOG: &str = "OFF_LIST_CATALOG";
pub const ERR_UI_BUNDLE_TAMPERED: &str = "UI_BUNDLE_TAMPERED";
pub const ERR_CHILD_MODULE_NOT_ALLOWED: &str = "CHILD_MODULE_NOT_ALLOWED";
pub const ERR_DOC_VERSION_CONFLICT: &str = "DOC_VERSION_CONFLICT";
pub const ERR_INPUT_LIMIT_EXCEEDED: &str = "INPUT_LIMIT_EXCEEDED";
pub const ERR_ROW_BUDGET_EXCEEDED: &str = "ROW_BUDGET_EXCEEDED";
pub const ERR_UNAUTHENTICATED: &str = "UNAUTHENTICATED";
pub const ERR_MFA_TOTP_REQUIRED: &str = "MFA_TOTP_REQUIRED";

/// Structured, currency-agnostic machine-readable error code for HTTP/MCP/API responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    // Generic category codes
    NotFound,
    ValidationError,
    AccessDenied,
    IntegrityViolation,
    StorageError,
    ContainerEngineError,
    ProtocolError,
    PaymentError,
    RateLimited,
    UpstreamLlmError,
    InternalError,

    // Domain-specific stable codes (Milestone C)
    InsufficientFunds,
    DailyActBreakerExceeded,
    AgentCeilingExceeded,
    AgentCredentialRevoked,
    AgentCredentialExpired,
    IdempotencyConflict,
    OffListModule,
    OffListCatalog,
    UiBundleTampered,
    ChildModuleNotAllowed,
    DocVersionConflict,
    InputLimitExceeded,
    RowBudgetExceeded,
    Unauthenticated,
    /// The account requires a TOTP second factor for this request (`401`).
    MfaTotpRequired,
}

impl ErrorCode {
    /// Canonical `SCREAMING_SNAKE_CASE` wire code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => ERR_NOT_FOUND,
            Self::ValidationError => ERR_VALIDATION_ERROR,
            Self::AccessDenied => ERR_ACCESS_DENIED,
            Self::IntegrityViolation => ERR_INTEGRITY_VIOLATION,
            Self::StorageError => ERR_STORAGE_ERROR,
            Self::ContainerEngineError => ERR_CONTAINER_ENGINE_ERROR,
            Self::ProtocolError => ERR_PROTOCOL_ERROR,
            Self::PaymentError => ERR_PAYMENT_ERROR,
            Self::RateLimited => ERR_RATE_LIMITED,
            Self::UpstreamLlmError => ERR_UPSTREAM_LLM_ERROR,
            Self::InternalError => ERR_INTERNAL_ERROR,
            Self::InsufficientFunds => ERR_INSUFFICIENT_FUNDS,
            Self::DailyActBreakerExceeded => ERR_DAILY_ACT_BREAKER_EXCEEDED,
            Self::AgentCeilingExceeded => ERR_AGENT_CEILING_EXCEEDED,
            Self::AgentCredentialRevoked => ERR_AGENT_CREDENTIAL_REVOKED,
            Self::AgentCredentialExpired => ERR_AGENT_CREDENTIAL_EXPIRED,
            Self::IdempotencyConflict => ERR_IDEMPOTENCY_CONFLICT,
            Self::OffListModule => ERR_OFF_LIST_MODULE,
            Self::OffListCatalog => ERR_OFF_LIST_CATALOG,
            Self::UiBundleTampered => ERR_UI_BUNDLE_TAMPERED,
            Self::ChildModuleNotAllowed => ERR_CHILD_MODULE_NOT_ALLOWED,
            Self::DocVersionConflict => ERR_DOC_VERSION_CONFLICT,
            Self::InputLimitExceeded => ERR_INPUT_LIMIT_EXCEEDED,
            Self::RowBudgetExceeded => ERR_ROW_BUDGET_EXCEEDED,
            Self::Unauthenticated => ERR_UNAUTHENTICATED,
            Self::MfaTotpRequired => ERR_MFA_TOTP_REQUIRED,
        }
    }

    /// Parse a wire error code string into its typed `ErrorCode`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            ERR_NOT_FOUND => Some(Self::NotFound),
            ERR_VALIDATION_ERROR => Some(Self::ValidationError),
            ERR_ACCESS_DENIED => Some(Self::AccessDenied),
            ERR_INTEGRITY_VIOLATION => Some(Self::IntegrityViolation),
            ERR_STORAGE_ERROR => Some(Self::StorageError),
            ERR_CONTAINER_ENGINE_ERROR => Some(Self::ContainerEngineError),
            ERR_PROTOCOL_ERROR => Some(Self::ProtocolError),
            ERR_PAYMENT_ERROR => Some(Self::PaymentError),
            ERR_RATE_LIMITED => Some(Self::RateLimited),
            ERR_UPSTREAM_LLM_ERROR => Some(Self::UpstreamLlmError),
            ERR_INTERNAL_ERROR => Some(Self::InternalError),
            ERR_INSUFFICIENT_FUNDS => Some(Self::InsufficientFunds),
            ERR_DAILY_ACT_BREAKER_EXCEEDED => Some(Self::DailyActBreakerExceeded),
            ERR_AGENT_CEILING_EXCEEDED => Some(Self::AgentCeilingExceeded),
            ERR_AGENT_CREDENTIAL_REVOKED => Some(Self::AgentCredentialRevoked),
            ERR_AGENT_CREDENTIAL_EXPIRED => Some(Self::AgentCredentialExpired),
            ERR_IDEMPOTENCY_CONFLICT => Some(Self::IdempotencyConflict),
            ERR_OFF_LIST_MODULE => Some(Self::OffListModule),
            ERR_OFF_LIST_CATALOG => Some(Self::OffListCatalog),
            ERR_UI_BUNDLE_TAMPERED => Some(Self::UiBundleTampered),
            ERR_CHILD_MODULE_NOT_ALLOWED => Some(Self::ChildModuleNotAllowed),
            ERR_DOC_VERSION_CONFLICT => Some(Self::DocVersionConflict),
            ERR_INPUT_LIMIT_EXCEEDED => Some(Self::InputLimitExceeded),
            ERR_ROW_BUDGET_EXCEEDED => Some(Self::RowBudgetExceeded),
            ERR_UNAUTHENTICATED => Some(Self::Unauthenticated),
            ERR_MFA_TOTP_REQUIRED => Some(Self::MfaTotpRequired),
            _ => None,
        }
    }

    /// Parse a domain-specific `ErrorCode` prefix from an error message (`"<CODE>: ..."` or `"<CODE>"`).
    pub(crate) fn from_message_prefix(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        let token = trimmed.split_once(':').map_or(trimmed, |(head, _)| head.trim());
        let parsed = Self::parse(token)?;
        parsed.is_domain_specific().then_some(parsed)
    }

    /// Returns `true` for fine-grained domain codes (not the generic `CompassError` variant fallbacks).
    pub const fn is_domain_specific(self) -> bool {
        matches!(
            self,
            Self::InsufficientFunds
                | Self::DailyActBreakerExceeded
                | Self::AgentCeilingExceeded
                | Self::AgentCredentialRevoked
                | Self::AgentCredentialExpired
                | Self::IdempotencyConflict
                | Self::OffListModule
                | Self::OffListCatalog
                | Self::UiBundleTampered
                | Self::ChildModuleNotAllowed
                | Self::DocVersionConflict
                | Self::InputLimitExceeded
                | Self::RowBudgetExceeded
                | Self::Unauthenticated
                | Self::MfaTotpRequired
        )
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn format_coded(code: ErrorCode, detail: impl Into<String>) -> String {
    let detail = detail.into();
    let trimmed = detail.trim();
    let prefix = format!("{}:", code.as_str());
    if trimmed.is_empty() {
        code.as_str().to_owned()
    } else if trimmed == code.as_str() || trimmed.starts_with(&prefix) {
        trimmed.to_owned()
    } else {
        format!("{}: {trimmed}", code.as_str())
    }
}

impl CompassError {
    /// Construct a `CompassError` carrying a canonical stable [`ErrorCode`] prefix inside the
    /// appropriate HTTP-mapped variant.
    pub fn coded(code: ErrorCode, detail: impl Into<String>) -> Self {
        let msg = if code.is_domain_specific() {
            format_coded(code, detail)
        } else {
            detail.into()
        };
        match code {
            ErrorCode::NotFound => Self::NotFound(msg),
            ErrorCode::ValidationError | ErrorCode::InputLimitExceeded => Self::Validation(msg),
            ErrorCode::AccessDenied
            | ErrorCode::Unauthenticated
            | ErrorCode::MfaTotpRequired
            | ErrorCode::AgentCredentialRevoked
            | ErrorCode::AgentCredentialExpired
            | ErrorCode::OffListModule
            | ErrorCode::OffListCatalog
            | ErrorCode::ChildModuleNotAllowed => Self::AccessDenied(msg),
            ErrorCode::IntegrityViolation
            | ErrorCode::UiBundleTampered
            | ErrorCode::DocVersionConflict
            | ErrorCode::IdempotencyConflict => Self::IntegrityViolation(msg),
            ErrorCode::StorageError => Self::Storage(msg),
            ErrorCode::ContainerEngineError => Self::ContainerEngine(msg),
            ErrorCode::ProtocolError => Self::Protocol(msg),
            ErrorCode::PaymentError
            | ErrorCode::InsufficientFunds
            | ErrorCode::DailyActBreakerExceeded
            | ErrorCode::AgentCeilingExceeded => Self::Payment(msg),
            ErrorCode::RateLimited | ErrorCode::RowBudgetExceeded => Self::RateLimited(msg),
            ErrorCode::UpstreamLlmError => Self::UpstreamLlm(msg),
            ErrorCode::InternalError => Self::Internal(msg),
        }
    }

    pub fn insufficient_funds(detail: impl Into<String>) -> Self {
        Self::coded(ErrorCode::InsufficientFunds, detail)
    }

    pub fn daily_act_breaker_exceeded(detail: impl Into<String>) -> Self {
        Self::coded(ErrorCode::DailyActBreakerExceeded, detail)
    }

    pub fn agent_ceiling_exceeded(detail: impl Into<String>) -> Self {
        Self::coded(ErrorCode::AgentCeilingExceeded, detail)
    }

    pub fn agent_credential_revoked(detail: impl Into<String>) -> Self {
        Self::coded(ErrorCode::AgentCredentialRevoked, detail)
    }

    pub fn agent_credential_expired(detail: impl Into<String>) -> Self {
        Self::coded(ErrorCode::AgentCredentialExpired, detail)
    }

    pub fn idempotency_conflict(detail: impl Into<String>) -> Self {
        Self::coded(ErrorCode::IdempotencyConflict, detail)
    }

    pub fn off_list_module(detail: impl Into<String>) -> Self {
        Self::coded(ErrorCode::OffListModule, detail)
    }

    pub fn off_list_catalog(detail: impl Into<String>) -> Self {
        Self::coded(ErrorCode::OffListCatalog, detail)
    }

    pub fn ui_bundle_tampered(detail: impl Into<String>) -> Self {
        Self::coded(ErrorCode::UiBundleTampered, detail)
    }

    pub fn child_module_not_allowed(detail: impl Into<String>) -> Self {
        Self::coded(ErrorCode::ChildModuleNotAllowed, detail)
    }

    pub fn doc_version_conflict(detail: impl Into<String>) -> Self {
        Self::coded(ErrorCode::DocVersionConflict, detail)
    }

    pub fn input_limit_exceeded(detail: impl Into<String>) -> Self {
        Self::coded(ErrorCode::InputLimitExceeded, detail)
    }

    pub fn row_budget_exceeded(detail: impl Into<String>) -> Self {
        Self::coded(ErrorCode::RowBudgetExceeded, detail)
    }

    pub fn unauthenticated(detail: impl Into<String>) -> Self {
        Self::coded(ErrorCode::Unauthenticated, detail)
    }

    pub fn mfa_totp_required(detail: impl Into<String>) -> Self {
        Self::coded(ErrorCode::MfaTotpRequired, detail)
    }

    /// Extract the stable machine-readable [`ErrorCode`] for this error.
    ///
    /// When the error carries a known domain code prefix (e.g. `INSUFFICIENT_FUNDS: ...` or
    /// `AGENT_CEILING_EXCEEDED: ...`), that specific code is returned; otherwise the variant's
    /// canonical generic [`ErrorCode`] is returned.
    pub fn error_code(&self) -> ErrorCode {
        let raw = self.raw_message();
        if let Some(domain_code) = ErrorCode::from_message_prefix(raw) {
            return domain_code;
        }
        match self {
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::Validation(_) => ErrorCode::ValidationError,
            Self::AccessDenied(_) => ErrorCode::AccessDenied,
            Self::IntegrityViolation(_) => ErrorCode::IntegrityViolation,
            Self::Storage(_) => ErrorCode::StorageError,
            Self::ContainerEngine(_) => ErrorCode::ContainerEngineError,
            Self::Protocol(_) => ErrorCode::ProtocolError,
            Self::Payment(_) => ErrorCode::PaymentError,
            Self::RateLimited(_) => ErrorCode::RateLimited,
            Self::UpstreamLlm(_) => ErrorCode::UpstreamLlmError,
            Self::Internal(_) => ErrorCode::InternalError,
        }
    }

    /// Returns `true` when this error matches the exact stable [`ErrorCode`].
    pub fn is_code(&self, code: ErrorCode) -> bool {
        self.error_code() == code
    }

    /// Borrow the underlying inner message string without the `thiserror` variant prefix.
    pub fn raw_message(&self) -> &str {
        match self {
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
            | Self::Internal(m) => m.as_str(),
        }
    }
}
