//! Money, in integer micro-USD.
//!
//! ADR-0008: never floating point. This matches the platform's existing
//! `gateway_ratelimit_spend_micro_usd` series so Copilot spend, Foundry estimated
//! cost and gateway spend are directly comparable in one Grafana panel.

/// A monetary amount in micro-USD (1 USD == 1_000_000).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct MicroUsd(pub i64);

impl MicroUsd {
    /// Micro-USD in one USD.
    pub const PER_USD: i64 = 1_000_000;

    /// Build an amount from whole USD.
    #[must_use]
    pub const fn from_usd(usd: i64) -> Self {
        Self(usd * Self::PER_USD)
    }
}

impl std::fmt::Display for MicroUsd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "${}.{:06}",
            self.0 / Self::PER_USD,
            (self.0 % Self::PER_USD).abs()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::MicroUsd;

    #[test]
    fn from_usd_scales_by_one_million() {
        assert_eq!(MicroUsd::from_usd(15), MicroUsd(15_000_000));
    }

    #[test]
    fn display_keeps_six_fractional_digits() {
        assert_eq!(MicroUsd(1_234_567).to_string(), "$1.234567");
    }
}
