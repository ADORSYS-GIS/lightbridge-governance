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

#[cfg(test)]
mod tests;

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
    /// (the loopback substitute). `manual`'s Copilot path (the file exporter
    /// plus `copilot push`) has no reason to run under `daemon`, where
    /// [`Self::copilot_otlp_direct`] is Copilot's path instead -- see
    /// [`crate::otel::OtelSettings::copilot_drain_available`], which this
    /// feeds directly.
    pub copilot_drain_available: bool,
    /// `true` under `daemon` with an endpoint configured (#272 AC3): Copilot
    /// points its OWN `otlp-http` exporter at the loopback daemon, needing no
    /// credential at all -- the reason that exporter was abandoned in favour
    /// of the file (a static header syncing off-machine via Settings Sync)
    /// does not apply to a loopback endpoint nothing can reach without also
    /// being on this machine. Mutually exclusive with
    /// [`Self::copilot_drain_available`] by construction: exactly one of the
    /// two Copilot paths is active per profile, never both, never neither
    /// while an endpoint is configured.
    pub copilot_otlp_direct: bool,
}

impl TelemetryWiring {
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
            copilot_otlp_direct: is_daemon && config.otel_endpoint.is_some(),
        }
    }
}
