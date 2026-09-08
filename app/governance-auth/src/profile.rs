//! The `daemon`/`manual` telemetry profile ADR-0016 makes a first-class,
//! persisted setting -- resolved through ADR-0012 Decision 2's five layers
//! by [`crate::config::OauthConfigArgs::resolve`], exactly like
//! `otel_endpoint` and everything else in that struct -- rather than an
//! implicit mode a developer discovers by reading which flags they happened
//! to pass.
//!
//! ADR-0016 makes `Daemon` the eventual compiled default. [`Profile::default`]
//! is `Manual` for now, deliberately diverging from the ADR. Both of the
//! original preconditions have landed in code: the daemon itself
//! (`serve --otel`, #268) and Copilot's own `otlp-http` exporter rewired onto
//! it (#272). Defaulting to `Daemon` before #268 landed would have moved
//! every developer who upgrades and re-runs `configure` without an explicit
//! `--profile` onto wiring this repo could not yet serve at all -- three P0s
//! from one review, confirmed live against a real machine: the drain that
//! delivers telemetry today torn down, every client's OTLP export redirected
//! to a port nothing listens on, and the daemon service entering a permanent
//! `Restart=on-failure` crash loop, all silently.
//!
//! Now that both preconditions are code-complete, the reason the default
//! still does not flip is #272's own one flagged, load-bearing assumption:
//! whether Copilot's `otlp-http` exporter actually accepts a plain-`http://`
//! loopback address from a real VS Code install has been verified only at
//! the config-file level this repo can test, not against the real client.
//! Flipping the default before that is field-confirmed would move every
//! such developer onto the unconfirmed path silently, the same "moved before
//! this repo can fully serve it" failure #268's own gap caused. There is no
//! CLI-introspectable tripwire for "the otlp-http assumption is confirmed"
//! the way [`crate::cli::invoke::serve_otel_is_supported`] answers "#268 has
//! landed" -- there is nothing short of a real VS Code install this binary
//! can drive to check it against -- so flipping this back to `Self::Daemon`
//! is a decision to make explicitly once that confirmation exists, not an
//! automatic one.

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

    /// Not `Daemon`, even though that's ADR-0016's eventual default, and
    /// even though both #268 and #272 have now landed -- see this module's
    /// doc for the one remaining reason (Copilot's otlp-http-at-loopback
    /// assumption, not yet field-confirmed) the two stay deliberately out
    /// of sync.
    #[test]
    fn the_compiled_default_stays_manual_pending_field_confirmation() {
        assert_eq!(Profile::default(), Profile::Manual);
    }
}
