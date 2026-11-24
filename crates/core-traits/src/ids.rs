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

/// Integer minor units of compute value in euros (`1 EUR = 1_000 milli-EUR`; `10 milli-EUR = €0.01`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MilliEur(pub i64);

impl MilliEur {
    /// Zero balance.
    pub const ZERO: Self = Self(0);

    /// Number of milli-EUR per whole euro.
    pub const PER_EUR: i64 = 1_000;

    /// Construct from whole EUR (`1 EUR = 1_000 milli-EUR`).
    pub const fn from_eur(whole_eur: i64) -> Self {
        Self(whole_eur * Self::PER_EUR)
    }

    /// Construct from euro cents (`1 cent = 10 milli-EUR`).
    pub const fn from_cents(cents: i64) -> Self {
        Self(cents * 10)
    }

    /// Construct from raw milli-EUR minor units.
    pub const fn from_milli(milli: i64) -> Self {
        Self(milli)
    }

    /// Return raw milli-EUR minor units as `i64`.
    pub const fn as_milli(self) -> i64 {
        self.0
    }

    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    pub const fn is_positive(self) -> bool {
        self.0 > 0
    }

    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    pub const fn checked_add(self, rhs: Self) -> Option<Self> {
        match self.0.checked_add(rhs.0) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    pub const fn checked_sub(self, rhs: Self) -> Option<Self> {
        match self.0.checked_sub(rhs.0) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    pub const fn saturating_add(self, rhs: Self) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }

    pub const fn saturating_sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }

    pub const fn checked_neg(self) -> Option<Self> {
        match self.0.checked_neg() {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    pub const fn abs(self) -> Self {
        Self(self.0.abs())
    }

    /// Parse a non-negative decimal EUR amount string (`"12.34"`, `"75"`, `"1.5"`, `"99.999"`)
    /// into [`MilliEur`]. Rejects signs, empty input, non-ASCII digits, >3 fractional digits, and overflow.
    pub fn parse_eur_decimal(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('+')
            || trimmed.starts_with('-')
            || trimmed.chars().any(|c| !c.is_ascii_digit() && c != '.')
        {
            return Err(format!("invalid EUR amount value '{raw}'"));
        }
        let (whole_str, frac_str) = match trimmed.split_once('.') {
            Some((w, f)) => {
                if f.contains('.') || w.is_empty() || f.is_empty() || f.len() > 3 {
                    return Err(format!(
                        "invalid EUR decimal precision in '{raw}' (expected 1..=3 fractional digits)"
                    ));
                }
                (w, f)
            }
            None => (trimmed, ""),
        };
        let whole: i64 = whole_str
            .parse()
            .map_err(|_| format!("EUR whole amount out of range in '{raw}'"))?;
        let frac: i64 = match frac_str.len() {
            0 => 0,
            1 => frac_str
                .parse::<i64>()
                .map_err(|_| format!("invalid EUR fraction in '{raw}'"))?
                * 100,
            2 => frac_str
                .parse::<i64>()
                .map_err(|_| format!("invalid EUR fraction in '{raw}'"))?
                * 10,
            3 => frac_str
                .parse::<i64>()
                .map_err(|_| format!("invalid EUR fraction in '{raw}'"))?,
            _ => unreachable!(),
        };
        let milli = whole
            .checked_mul(Self::PER_EUR)
            .and_then(|w| w.checked_add(frac))
            .ok_or_else(|| format!("EUR amount overflow in '{raw}'"))?;
        Ok(Self(milli))
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

    /// Format as euros to the cent (e.g., `€0.00`, `€0.05`, `€12.34`, `-€0.05`).
    pub fn display_eur(self) -> String {
        let (sign, whole, cents) = self.rounded_cents();
        format!("{sign}€{whole}.{cents:02}")
    }
}

impl fmt::Display for MilliEur {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_eur())
    }
}

impl From<i64> for MilliEur {
    fn from(milli: i64) -> Self {
        Self(milli)
    }
}

impl From<MilliEur> for i64 {
    fn from(amount: MilliEur) -> Self {
        amount.0
    }
}

impl std::ops::Add for MilliEur {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl std::ops::Sub for MilliEur {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 - rhs.0)
    }
}

impl std::ops::Neg for MilliEur {
    type Output = Self;

    fn neg(self) -> Self::Output {
        Self(-self.0)
    }
}

impl std::ops::AddAssign for MilliEur {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl std::ops::SubAssign for MilliEur {
    fn sub_assign(&mut self, rhs: Self) {
        self.0 -= rhs.0;
    }
}

impl std::iter::Sum for MilliEur {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, |acc, item| acc + item)
    }
}

#[cfg(test)]
mod milli_eur_tests {
    use super::MilliEur;

    #[test]
    fn display_eur_formats_euros_to_the_cent() {
        assert_eq!(MilliEur::ZERO.display_eur(), "€0.00");
        assert_eq!(MilliEur::from_milli(50).display_eur(), "€0.05");
        assert_eq!(MilliEur::from_milli(1_000).display_eur(), "€1.00");
        assert_eq!(MilliEur::from_milli(12_340).display_eur(), "€12.34");
        assert_eq!(MilliEur::from_milli(12_345).display_eur(), "€12.35");
        assert_eq!(MilliEur::from_milli(-50).display_eur(), "-€0.05");
        assert_eq!(MilliEur::from_milli(-4).display_eur(), "€0.00");
        assert_eq!(MilliEur::from_eur(250).display_eur(), "€250.00");
        assert_eq!(MilliEur::from_cents(1234).to_string(), "€12.34");
    }

    #[test]
    fn milli_eur_arithmetic_serde_and_parse_decimal() {
        let a = MilliEur::from_eur(10);
        let b = MilliEur::from_cents(250);
        assert_eq!(a + b, MilliEur::from_milli(12_500));
        assert_eq!(a - b, MilliEur::from_milli(7_500));
        assert_eq!(-b, MilliEur::from_milli(-2_500));
        assert_eq!((-b).abs(), b);
        assert_eq!(
            [a, b].into_iter().sum::<MilliEur>(),
            MilliEur::from_milli(12_500)
        );
        assert_eq!(serde_json::to_string(&a).unwrap(), "10000");
        assert_eq!(serde_json::from_str::<MilliEur>("10000").unwrap(), a);

        assert_eq!(MilliEur::parse_eur_decimal("12.34").unwrap(), MilliEur::from_milli(12_340));
        assert_eq!(MilliEur::parse_eur_decimal("99.999").unwrap(), MilliEur::from_milli(99_999));
        assert_eq!(MilliEur::parse_eur_decimal("75").unwrap(), MilliEur::from_eur(75));
        assert_eq!(MilliEur::parse_eur_decimal("1.5").unwrap(), MilliEur::from_milli(1_500));
        for bad in ["", "-1.00", "+1.00", "1.2345", ".5", "5.", "1.2.3", "abc"] {
            assert!(MilliEur::parse_eur_decimal(bad).is_err(), "expected error for '{bad}'");
        }
    }
}
