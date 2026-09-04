//! The profile-dependent half of [`crate::otel::OtelSettings`] (ADR-0016 /
//! #270 AC1/AC2): the endpoint every client is told to use, the credential
//! (if any) it carries, and whether Claude Code's refresh helper is wired.
//!
//! Split out of [`super`] rather than left inline in `apply_telemetry`, for
//! two reasons. First, testability: left inline, the profile branch is only
//! reachable through `apply_telemetry`'s `$HOME`-writing path, which needs a
//! real filesystem to exercise at all -- the same reason
//! `schedule::systemd::classify` is a free function rather than a closure
//! inside `survey`. Second, room: `oauth/mod.rs` was already the size it is
//! before this existed, and growing it further runs against the same
//! 200-line ratchet (#162) this binary's own CI now enforces on every OTHER
//! file in the tree.

use crate::{config::OauthConfig, otel_port, redacted::Redacted};

pub(super) struct TelemetryWiring {
    /// `None`/`Some` tracks `config.otel_endpoint` unchanged -- "no
    /// endpoint configured" stays "no telemetry wiring at all" under either
    /// profile (`vscode::configure`'s own `settings.endpoint.is_none()`
    /// check depends on that). Only the `Some` *value* is substituted: the
    /// loopback receiver under `daemon`, the real collector under `manual`.
    pub endpoint: Option<String>,
    /// `None` under `daemon` regardless of `--otel-token`: the daemon mints
    /// its own fresh bearer (#268 AC3), so a client-side credential would be
    /// one MORE long-lived secret, not fewer -- exactly what ADR-0016 exists
    /// to remove.
    pub token: Option<Redacted<String>>,
    /// `false` under `daemon`: there is no credential to refresh when the
    /// client sends none at all (see `token`). The caller still ANDs this
    /// with whether telemetry was requested at all -- this field alone
    /// doesn't know whether an endpoint was configured, only what the
    /// profile implies if one was.
    pub wants_headers_helper: bool,
    /// `false` under `daemon`, `endpoint.is_some()` under `manual` --
    /// distinct from `endpoint` itself, which is `Some` under `daemon` too
    /// (the loopback substitute). Copilot has no path to the daemon yet
    /// (#272), so `daemon` must turn its file exporter OFF, not point it at
    /// an endpoint nothing drains -- see
    /// [`crate::otel::OtelSettings::copilot_drain_available`], which this
    /// feeds directly.
    pub copilot_drain_available: bool,
}

impl TelemetryWiring {
    /// `config.profile == Daemon` is read as-is, with no `serve_otel_is_supported()`
    /// re-check (#280 review round 2) -- `apply_telemetry`'s chokepoint
    /// already refuses the call before this runs on a build that can't
    /// serve `Daemon`, so re-checking here would be dead code.
    pub fn resolve(config: &OauthConfig) -> Self {
        let is_daemon = config.profile == crate::profile::Profile::Daemon;
        let endpoint = config.otel_endpoint.as_ref().map(|real| {
            if is_daemon {
                otel_port::OTEL_LOOPBACK_ENDPOINT.to_owned()
            } else {
                real.clone()
            }
        });
        let token = (!is_daemon)
            .then(|| config.otel_token.clone().map(Redacted::new))
            .flatten();
        Self {
            endpoint,
            token,
            wants_headers_helper: !is_daemon,
            copilot_drain_available: !is_daemon && config.otel_endpoint.is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> OauthConfig {
        OauthConfig {
            issuer: "https://issuer.example.com".to_owned(),
            client_id: "client".to_owned(),
            scopes: "openid".to_owned(),
            audience: None,
            otel_endpoint: None,
            otel_token: None,
            gateway_url: None,
            profile: crate::profile::Profile::Daemon,
            profile_explicit: Some(crate::profile::Profile::Daemon),
            copilot_spool_path: None,
            otel_headers_debounce_ms: 240_000,
            open_browser: false,
            token_exchange: None,
        }
    }

    /// ADR-0016 / #270 AC1: the endpoint substitution and the credential
    /// suppression, both in one assertion -- the two are meant to change
    /// together (a loopback endpoint with a static bearer attached would be
    /// the daemon's whole point undone).
    #[test]
    fn daemon_profile_points_at_loopback_and_carries_no_credential() {
        let config = OauthConfig {
            otel_endpoint: Some("https://otel.example".to_owned()),
            otel_token: Some("static-secret".to_owned()),
            profile: crate::profile::Profile::Daemon,
            ..config()
        };
        let wiring = TelemetryWiring::resolve(&config);
        assert_eq!(
            wiring.endpoint.as_deref(),
            Some(otel_port::OTEL_LOOPBACK_ENDPOINT)
        );
        assert!(
            wiring.token.is_none(),
            "a static credential must never reach a client under `daemon`"
        );
        assert!(!wiring.wants_headers_helper);
    }

    /// ADR-0016 / #270 AC2: `manual` must reproduce today's behaviour
    /// EXACTLY -- falsified by asserting the real endpoint and the real
    /// token survive unchanged, not just that they are "present".
    #[test]
    fn manual_profile_reproduces_the_real_endpoint_and_token() {
        let config = OauthConfig {
            otel_endpoint: Some("https://otel.example".to_owned()),
            otel_token: Some("static-secret".to_owned()),
            profile: crate::profile::Profile::Manual,
            ..config()
        };
        let wiring = TelemetryWiring::resolve(&config);
        assert_eq!(wiring.endpoint.as_deref(), Some("https://otel.example"));
        assert_eq!(
            wiring.token.map(|token| token.expose().clone()),
            Some("static-secret".to_owned())
        );
        assert!(wiring.wants_headers_helper);
    }

    /// "No endpoint configured" must stay "no telemetry wiring at all"
    /// under EITHER profile -- `vscode::configure`'s own
    /// `settings.endpoint.is_none()` check depends on this holding, not
    /// just on `daemon`'s substitution not accidentally manufacturing one.
    #[test]
    fn no_endpoint_configured_resolves_to_no_endpoint_under_either_profile() {
        for profile in [
            crate::profile::Profile::Daemon,
            crate::profile::Profile::Manual,
        ] {
            let config = OauthConfig {
                otel_endpoint: None,
                profile,
                ..config()
            };
            assert!(
                TelemetryWiring::resolve(&config).endpoint.is_none(),
                "{profile} must not manufacture an endpoint nothing configured"
            );
        }
    }

    /// Confirmed live: without this, `daemon` substitutes a non-empty
    /// loopback `endpoint`, `vscode::configure`'s `endpoint.is_none()` gate
    /// reads that as "turn the file exporter on", and Copilot's spool grows
    /// forever with the drain that used to empty it removed (#272 has not
    /// rewired Copilot onto the daemon). `endpoint` itself must still carry
    /// the loopback value -- Claude Code and Codex still need it -- so the
    /// fix is a second signal, not touching `endpoint`.
    #[test]
    fn daemon_profile_has_an_endpoint_but_no_copilot_drain() {
        let config = OauthConfig {
            otel_endpoint: Some("https://otel.example".to_owned()),
            profile: crate::profile::Profile::Daemon,
            ..config()
        };
        let wiring = TelemetryWiring::resolve(&config);
        assert!(
            wiring.endpoint.is_some(),
            "Claude Code and Codex still need the loopback endpoint"
        );
        assert!(
            !wiring.copilot_drain_available,
            "nothing drains Copilot's spool under `daemon` until #272 lands"
        );
    }

    #[test]
    fn manual_profile_has_a_copilot_drain_exactly_when_an_endpoint_is_configured() {
        for (endpoint, expected) in [(Some("https://otel.example"), true), (None, false)] {
            let config = OauthConfig {
                otel_endpoint: endpoint.map(str::to_owned),
                profile: crate::profile::Profile::Manual,
                ..config()
            };
            assert_eq!(
                TelemetryWiring::resolve(&config).copilot_drain_available,
                expected,
                "endpoint = {endpoint:?}"
            );
        }
    }
}
