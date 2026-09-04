//! The `daemon`/`manual` telemetry profile ADR-0016 makes a first-class,
//! persisted setting -- resolved through ADR-0012 Decision 2's five layers
//! by [`crate::config::OauthConfigArgs::resolve`], exactly like
//! `otel_endpoint` and everything else in that struct -- rather than an
//! implicit mode a developer discovers by reading which flags they happened
//! to pass.
//!
//! ADR-0016 makes `Daemon` the eventual compiled default. [`Profile::default`]
//! is `Manual` for now, deliberately diverging from the ADR, pending BOTH of:
//! the daemon itself (`serve --otel`, #268 -- landed) and Copilot's rewiring
//! onto it (#272 -- not yet). Defaulting to `Daemon` before both land would
//! move every developer who upgrades and re-runs `configure` without an
//! explicit `--profile` onto wiring this repo cannot yet fully serve -- three
//! P0s from one review, confirmed live against a real machine, if flipped
//! before #268: the drain that delivers telemetry today is torn down, every
//! client's OTLP export is redirected to a port nothing listens on, and the
//! daemon service that's supposed to replace them enters a permanent
//! `Restart=on-failure` crash loop, all silently. #268 landing alone removes
//! only the crash-loop third of that: with no #272, `daemon` still tears down
//! the working Copilot drain and installs a service with no path to the
//! Copilot spool, growing it forever with nothing to consume it (#280 review,
//! the `schedule/daemon/mod.rs` finding). `oauth::apply_telemetry`'s
//! chokepoint (#280 review round 2) refuses `daemon` outright when #268 is
//! missing; nothing yet gates on #272 the same way, so the default staying
//! `Manual` is still load-bearing on its own, not just belt-and-suspenders.
//! There is no CLI-introspectable tripwire for "#272 has landed" the way
//! [`crate::cli::invoke::serve_otel_is_supported`] answers "#268 has landed"
//! -- flipping this back to `Self::Daemon` is a decision to make explicitly
//! once #272 merges, not an automatic one.

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

/// See this module's doc: `Manual` until #268/#272 land, not `Daemon` yet.
/// Explicit rather than `#[derive(Default)]` so that citation sits next to
/// the choice, not implied by variant order.
impl Default for Profile {
    fn default() -> Self {
        Self::Manual
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

    /// Not `Daemon`, even though that's ADR-0016's eventual default -- see
    /// this module's doc for why the two are deliberately out of sync right
    /// now. `cli::invoke::tests::serve_otel_is_not_yet_a_real_command` is
    /// what flips this back once #268 lands.
    #[test]
    fn the_compiled_default_is_manual_until_268_and_272_land() {
        assert_eq!(Profile::default(), Profile::Manual);
    }
}
