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
define_id!(SessionId, "Durable agent conversation session identifier.");
define_id!(RunId, "Plugin container execution run identifier.");
define_id!(PluginId, "Universal BioCompass plugin identifier (`namespace/name`).");
define_id!(CatalogId, "Immutable scientific Parquet catalog identifier.");
define_id!(NodeId, "Froglet execution node BIP340 x-only public key or node identifier.");

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
