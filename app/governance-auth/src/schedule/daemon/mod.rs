//! The daemon (`serve --otel`) as a systemd/launchd service -- ADR-0016,
//! #260/#270.
//!
//! A sibling module to [`super::systemd`]/[`super::launchd`], not a branch
//! inside them: those two render a `Type=oneshot` unit driven by a
//! `.timer`, and this one renders a persistent `Type=simple`/`KeepAlive`
//! unit with no timer at all -- different enough that folding both into one
//! file would blow past the 200-line ceiling (`systemd.rs` is 170/200,
//! `launchd.rs` 154/200 already, both with only a few dozen lines of
//! headroom). The install/remove/survey *pattern* is copied from those two,
//! not reimplemented from scratch; the low-level `run()` (this module's
//! grandparent, [`super::super`]) and `systemd::classify` ARE reused, not
//! duplicated -- see this module's own `systemd` submodule.
//!
//! ## Installed by profile, not by client
//!
//! Unlike the Copilot drain -- which [`crate::optout::ClientOptOut`] can
//! leave alone with `--no-vscode` -- this is shared infrastructure every
//! client's telemetry passes through under the `daemon` profile, so it is
//! not part of that opt-out. [`apply`] installs when
//! [`crate::profile::Profile::Daemon`] is selected and a collector is
//! configured to forward to, and removes it otherwise: switching to
//! `manual`, or staying on `daemon` with no `--otel-endpoint`, both remove
//! it -- the same "nothing to point at" rule [`super::Invocation::resolve`]
//! already uses for the drain.

mod launchd;
mod systemd;
#[cfg(test)]
mod tests;

use std::path::Path;

use anyhow::{Result, bail};

use crate::{config::OauthConfig, profile::Profile};

/// systemd's unit stem -- `<DAEMON_UNIT>.service` is enabled directly, with
/// no `.timer` alongside it.
pub(crate) const DAEMON_UNIT: &str = "governance-auth-serve-otel";

/// launchd's reverse-DNS job label.
pub(crate) const DAEMON_LABEL: &str = "digital.camer.ai.governance-auth.serve-otel";

/// Exactly what the service runs. Mirrors [`super::Invocation`]'s shape but
/// not its `resolve()`: the drain needs a spool path and the Copilot argv
/// tail, and the daemon needs neither -- it forwards to the real collector
/// rather than draining a file.
#[derive(Debug, Clone)]
struct Invocation {
    program: String,
    args: Vec<String>,
}

impl Invocation {
    /// `Ok(None)` under `manual`, or under `daemon` with no collector
    /// configured to forward to -- both remove the service rather than
    /// install one pointing nowhere, exactly [`super::Invocation::resolve`]'s
    /// rule for the drain.
    ///
    /// `Err` when `daemon` is selected with a collector configured, but
    /// `serve_otel_supported` is `false` (#280 review, P1-1) -- the caller's
    /// answer to whether this build's `governance-auth` actually parses
    /// [`crate::cli::SERVE_OTEL`] yet, taken as a parameter (rather than
    /// asked for here via `crate::cli::serve_otel_is_supported` directly) so
    /// this decision is exercisable from a test without needing two builds
    /// of the binary. Before this check, `configure --profile daemon`
    /// installed a `Restart=on-failure` unit whose `ExecStart` was a command
    /// that did not exist on a pre-#268 build -- every client's telemetry
    /// silently stopped (the drain timer this replaces was removed) and the
    /// unit crash-looped every couple of seconds, with nothing telling the
    /// developer either was happening. "A service that refuses to boot beats
    /// one that boots with a mock credential" (AGENTS.md) applies exactly as
    /// well to a service that cannot boot at all.
    fn resolve(config: &OauthConfig, serve_otel_supported: bool) -> Result<Option<Self>> {
        if config.profile != Profile::Daemon {
            return Ok(None);
        }
        let Some(endpoint) = config.otel_endpoint.as_deref() else {
            return Ok(None);
        };
        if !serve_otel_supported {
            bail!(
                "refusing to install the `daemon` profile's service: this build of \
                 governance-auth does not have `serve --otel` yet (issue #268). Installing it \
                 anyway would remove the Copilot drain timer and leave every client's telemetry \
                 unwired, with a service that can only crash-loop. Upgrade to a build that has \
                 #268, or run `configure --profile manual` instead."
            );
        }
        Ok(Some(Self {
            program: crate::otel::binary_path(),
            args: vec![
                "--issuer".to_owned(),
                config.issuer.clone(),
                "--client-id".to_owned(),
                config.client_id.clone(),
                "--otel-endpoint".to_owned(),
                endpoint.to_owned(),
            ]
            .into_iter()
            .chain(crate::cli::SERVE_OTEL.iter().map(|word| (*word).to_owned()))
            .collect(),
        }))
    }
}

/// Installs the daemon service, or removes it when [`Invocation::resolve`]
/// finds nothing to forward to. Non-fatal by design, like [`super::apply`]:
/// every client config file is already written by the time this runs, so a
/// machine with no user systemd/launchd session must not turn a successful
/// `configure` into a failure -- see this function's caller,
/// `oauth::apply_telemetry`, which reports rather than propagates.
pub fn apply(home: &Path, config: &OauthConfig) -> Result<()> {
    let invocation = Invocation::resolve(config, crate::cli::serve_otel_is_supported())?;
    match (invocation, super::macos()) {
        (Some(invocation), true) => launchd::install(home, &invocation),
        (Some(invocation), false) => systemd::install(home, &invocation),
        (None, true) => launchd::remove(home),
        (None, false) => systemd::remove(home),
    }
}

/// What `dashboard::Daemon` will report (#271) -- reuses [`super::Schedule`]
/// rather than a parallel type, since the three-valued shape it promises is
/// identical for either unit.
#[expect(
    dead_code,
    reason = "wired in by #271 (status); no caller yet on this branch"
)]
pub fn survey(home: &Path) -> super::Schedule {
    if super::macos() {
        launchd::survey(home)
    } else {
        systemd::survey(home)
    }
}
