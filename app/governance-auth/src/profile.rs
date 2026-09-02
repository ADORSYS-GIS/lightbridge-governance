//! The `daemon`/`manual` telemetry profile ADR-0016 makes a first-class,
//! persisted setting -- resolved through ADR-0012 Decision 2's five layers
//! by [`crate::config::OauthConfigArgs::resolve`], exactly like
//! `otel_endpoint` and everything else in that struct -- rather than an
//! implicit mode a developer discovers by reading which flags they happened
//! to pass.
//!
//! `Daemon` is the compiled default: ADR-0016 adopts the local collector
//! daemon as the default telemetry path and keeps `Manual` -- today's direct
//! wiring -- as an explicitly selected, permanently supported profile, never
//! a deprecation shim. See that ADR's Decision section before changing which
//! variant [`Profile::default`] returns.

use std::{fmt, str::FromStr};

use anyhow::{Result, bail};

/// Which telemetry wiring `configure` writes. Copy: two bytes of discriminant,
/// no reason to borrow it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Every client points at the loopback daemon (`serve --otel`); no
    /// long-lived credential is ever written to a client's config.
    Daemon,
    /// Today's behaviour: direct exporters, the `copilot-push` timer, and a
    /// static `--otel-token` where a client needs one. The correct choice on
    /// a locked-down build agent, in a container, or anywhere a long-running
    /// user service is unwanted -- and what keeps working if the daemon is
    /// stopped.
    Manual,
}

impl Profile {
    /// The exact string persisted to a config file and accepted back by
    /// [`FromStr`] -- kept as one function so the two can't drift.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Manual => "manual",
        }
    }
}

/// ADR-0016's compiled default. Explicit rather than `#[derive(Default)]` so
/// the ADR citation sits next to the choice, not implied by variant order.
impl Default for Profile {
    fn default() -> Self {
        Self::Daemon
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Profile {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "daemon" => Ok(Self::Daemon),
            "manual" => Ok(Self::Manual),
            other => bail!("unknown profile `{other}`; expected `daemon` or `manual`"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_display_and_from_str() {
        for profile in [Profile::Daemon, Profile::Manual] {
            assert_eq!(profile.to_string().parse::<Profile>().unwrap(), profile);
        }
    }

    #[test]
    fn an_unrecognised_value_is_rejected_by_name() {
        // Falsification: assert the message actually names the bad input,
        // not just that parsing failed -- a generic error here would pass
        // even if the `other` branch's `{other}` were dropped.
        let error = "bogus".parse::<Profile>().unwrap_err();
        assert!(format!("{error}").contains("bogus"));
    }

    #[test]
    fn the_compiled_default_is_daemon_per_adr_0016() {
        assert_eq!(Profile::default(), Profile::Daemon);
    }
}
