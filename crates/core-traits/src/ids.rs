//! Strongly typed cross-boundary identifiers and credit units.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! define_id {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Construct a validated non-empty identifier.
            pub fn new(raw: impl Into<String>) -> Result<Self, String> {
                let value = raw.into();
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    return Err(format!("{} cannot be empty", stringify!($name)));
                }
                Ok(Self(trimmed.to_owned()))
            }

            /// Borrow the underlying string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.trim().to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value.trim().to_owned())
            }
        }
    };
}

define_id!(OrgId, "Organization tenant identifier.");
define_id!(ProjectId, "Workspace project folder identifier grouping sessions.");
define_id!(SessionId, "Durable agent conversation session identifier.");
define_id!(RunId, "Plugin container execution run identifier.");
define_id!(PluginId, "Universal BioCompass plugin identifier (`namespace/name`).");
define_id!(CatalogId, "Immutable scientific Parquet catalog identifier.");
define_id!(NodeId, "Froglet execution node BIP340 x-only public key or node identifier.");

impl CatalogId {
    /// Legacy table import and mutation must never create or change a managed private release.
    pub fn ensure_legacy_writable(&self) -> crate::CompassResult<()> {
        let id = self.as_str();
        if id.contains('\\') || id.split('/').any(|s| matches!(s, "" | "." | "..")) {
            return Err(crate::CompassError::Validation("invalid catalog path".into()));
        }
        if id.split_once('/').is_some_and(|(namespace, _)| namespace.eq_ignore_ascii_case("bc-private-v1")) {
            return Err(crate::CompassError::AccessDenied(format!(
                "catalog '{id}' requires the private dataset publication workflow"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod private_catalog_tests {
    use super::CatalogId;

    #[test]
    fn only_the_private_namespace_is_reserved_for_writes() {
        for id in ["bc-private-v1/study", "BC-PRIVATE-V1/study"] {
            assert!(matches!(CatalogId::from(id).ensure_legacy_writable(), Err(crate::CompassError::AccessDenied(_))));
        }
        for id in ["legacy", "org/study", "org/bc-private-v1", "bc-private-v10/study"] {
            CatalogId::from(id).ensure_legacy_writable().unwrap();
        }
        for id in ["/absolute", "org/../study", "org//study", "org/./study"] {
            assert!(CatalogId::from(id).ensure_legacy_writable().is_err());
        }
    }
}

/// Integer minor units of Alithea Compute Tokens (`1 ACT = 1_000 milli-ACT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CreditAmountAct(pub i64);

impl CreditAmountAct {
    /// Zero ACT credits.
    pub const ZERO: Self = Self(0);

    /// Construct from whole ACT tokens (`1 ACT = 1_000 milli-ACT`).
    pub const fn from_whole_act(whole: i64) -> Self {
        Self(whole * 1_000)
    }

    /// Construct from milli-ACT minor units.
    pub const fn from_milli_act(milli: i64) -> Self {
        Self(milli)
    }

    /// Return raw milli-ACT minor units.
    pub const fn milli_act(self) -> i64 {
        self.0
    }

    /// Format as human-readable ACT string (e.g., `12.50 ACT`).
    pub fn display_act(self) -> String {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        let whole = abs / 1_000;
        let frac = (abs % 1_000) / 10;
        format!("{sign}{whole}.{frac:02} ACT")
    }
}
