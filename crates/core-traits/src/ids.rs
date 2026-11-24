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

/// Integer minor units of compute value (`1 EUR = 1 ACT = 1_000 milli-units`; `10 milli-units = €0.01`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CreditAmountAct(pub i64);

impl CreditAmountAct {
    /// Zero balance.
    pub const ZERO: Self = Self(0);

    /// Construct from whole EUR / ACT units (`1 unit = 1_000 milli-units`).
    pub const fn from_whole_act(whole: i64) -> Self {
        Self(whole * 1_000)
    }

    /// Construct from milli-EUR / milli-ACT minor units.
    pub const fn from_milli_act(milli: i64) -> Self {
        Self(milli)
    }

    /// Return raw milli-EUR / milli-ACT minor units.
    pub const fn milli_act(self) -> i64 {
        self.0
    }

    fn rounded_cents(self) -> (&'static str, u64, u64) {
        let abs = self.0.unsigned_abs();
        let total_cents = abs.saturating_add(5) / 10;
        let sign = if self.0 < 0 && total_cents > 0 {
            "-"
        } else {
            ""
        };
        let whole = total_cents / 100;
        let cents = total_cents % 100;
        (sign, whole, cents)
    }

    /// Format as euros to the cent (e.g., `€1.00`, `€0.05`, `€12.34`).
    pub fn display_eur(self) -> String {
        let (sign, whole, cents) = self.rounded_cents();
        format!("{sign}€{whole}.{cents:02}")
    }

    /// Format as human-readable euro amount to the cent (`1 ACT = 1 EUR`, e.g., `€12.50`).
    ///
    /// For the laptop's local play ledger, use [`Self::display_local_credits`] so local play
    /// credits are never labelled as euros.
    pub fn display_act(self) -> String {
        self.display_eur()
    }

    /// Format for the local workstation play ledger (never labelled `€` or `EUR`).
    pub fn display_local_credits(self) -> String {
        let (sign, whole, cents) = self.rounded_cents();
        format!("{sign}{whole}.{cents:02} credits")
    }
}

#[cfg(test)]
mod credit_display_tests {
    use super::CreditAmountAct;

    #[test]
    fn display_act_formats_euros_to_the_cent() {
        assert_eq!(CreditAmountAct::ZERO.display_act(), "€0.00");
        assert_eq!(CreditAmountAct::from_milli_act(50).display_act(), "€0.05");
        assert_eq!(CreditAmountAct::from_milli_act(1_000).display_act(), "€1.00");
        assert_eq!(CreditAmountAct::from_milli_act(12_340).display_act(), "€12.34");
        assert_eq!(CreditAmountAct::from_milli_act(12_345).display_act(), "€12.35");
        assert_eq!(CreditAmountAct::from_milli_act(-50).display_act(), "-€0.05");
        assert_eq!(CreditAmountAct::from_milli_act(-4).display_act(), "€0.00");
        assert_eq!(CreditAmountAct::from_whole_act(250).display_eur(), "€250.00");
    }

    #[test]
    fn local_play_ledger_credits_are_never_labelled_euros() {
        for milli in [0, 50, 1_000, 12_340, 250_000, -5_000] {
            let formatted = CreditAmountAct::from_milli_act(milli).display_local_credits();
            assert!(!formatted.contains('€'), "unexpected euro symbol in {formatted}");
            assert!(!formatted.to_ascii_uppercase().contains("EUR"), "unexpected EUR label in {formatted}");
            assert!(formatted.ends_with(" credits"), "expected credits suffix in {formatted}");
        }
        assert_eq!(CreditAmountAct::from_milli_act(5_000).display_local_credits(), "5.00 credits");
        assert_eq!(CreditAmountAct::from_milli_act(50).display_local_credits(), "0.05 credits");
    }
}
