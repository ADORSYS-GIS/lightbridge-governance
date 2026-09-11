//! Re-applying `configure` after a real update, without requiring config.
//!
//! An upgrade can change the commands `configure` writes into other tools'
//! files (a renamed subcommand, a new flag) -- `docs/governance-auth/
//! troubleshooting.md`'s "Upgrading across the command rename" section is
//! the incident this closes: `self update` alone leaves the wiring stale
//! until a developer separately remembers to run `configure`, and the
//! failure is silent until a helper it wrote fails on its next invocation.
//!
//! [`reapply_configure_if_onboarded`] tries [`OauthConfigArgs::resolve`]
//! after a real (non-dry-run) update installs successfully, and runs
//! `configure` again only if that resolves -- i.e. only on a machine that
//! was already onboarded, where a config file (or the caller's own
//! flags/env) already supplies `issuer`/`client_id`. A resolve failure is
//! treated as "nothing to re-apply", not as an update failure: `super`'s own
//! module doc explains why resolving nothing else is deliberate here --
//! "the machine most likely to be updating is the one with no config yet"
//! -- and that stays true here: a fresh machine's `self update` behaves
//! exactly as before this existed.
//!
//! ## Replaying opt-outs, not overwriting them
//!
//! The first version of this shipped calling `configure` with
//! `ClientOptOut::default()` -- "opt out of nothing" -- unconditionally.
//! [lightbridge-governance#329's own review](https://github.com/ADORSYS-GIS/lightbridge-governance/pull/329)
//! caught the bug that is: a machine set up with `--no-vscode` (because VS
//! Code isn't installed there, or its settings are hand-managed) would have
//! `self update` silently re-enable it -- installing or removing the
//! Copilot drain schedule, rewriting keys the developer explicitly asked to
//! be left alone. `--no-*` are deliberately per-invocation CLI flags, never
//! part of `OauthConfigArgs`'s resolved five layers (see `ClientOptOut`'s
//! own module doc), so there was nothing to read them back from.
//!
//! `OauthConfig::last_no_claude` (and its three siblings) close that gap:
//! `config_persist::remember` now writes whatever `ClientOptOut` `configure`
//! or `login` was just run with, and [`reapply_configure`] rebuilds a
//! `ClientOptOut` from those four fields instead of `ClientOptOut::default`.
//! A machine that has never run `configure`/`login` on this build reads all
//! four as `false` (the same compiled default `ClientOptOut::default` was),
//! so this changes nothing for anyone who never used an opt-out flag.

use anyhow::Result;

use crate::{
    config::{OauthConfig, OauthConfigArgs},
    oauth,
    optout::ClientOptOut,
};

/// Re-runs `configure` after a real update, but only when this machine was
/// already onboarded -- see this module's own doc for why a resolve failure
/// is silence, not a warning: it is the ordinary state on a machine `self
/// update` is specifically designed to work on with no config at all.
pub(super) fn reapply_configure_if_onboarded(oauth: OauthConfigArgs) {
    reapply_configure(oauth.resolve());
}

/// Split from [`reapply_configure_if_onboarded`] so the decision itself --
/// configure only when something actually resolved -- is testable without a
/// real config file, a real `$HOME`, or a real write anywhere: an
/// already-`Err` `resolved` is checked before `oauth::configure` (which does
/// write real files) is ever reached.
fn reapply_configure(resolved: Result<OauthConfig>) {
    let Ok(config) = resolved else {
        return;
    };
    match oauth::configure(&config, optout_from(&config)) {
        Ok(()) => eprintln!("Re-applied `configure` for the new version."),
        Err(error) => eprintln!(
            "warning: updated the binary, but could not re-apply configuration ({error:#}) -- \
             run `governance-auth configure` by hand."
        ),
    }
}

/// The opt-out this machine was last configured with, not a fresh default --
/// see this module's own doc, "Replaying opt-outs, not overwriting them".
/// Split out purely so this mapping is testable without a real config
/// resolve or an `oauth::configure` call.
fn optout_from(config: &OauthConfig) -> ClientOptOut {
    ClientOptOut {
        claude: config.last_no_claude,
        codex: config.last_no_codex,
        vscode: config.last_no_vscode,
        codex_telemetry_only: config.last_codex_telemetry_only,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression this whole module exists to fix: reapplying `configure`
    /// must carry the machine's last opt-out choice forward, never silently
    /// reset to "opt out of nothing" the way the first version of this
    /// feature did (lightbridge-governance#329's own review).
    #[test]
    fn optout_from_config_carries_every_flag_forward() {
        let mut config = test_config();
        config.last_no_claude = true;
        config.last_no_vscode = true;
        // Left false, deliberately: proves the mapping is per-field, not a
        // single "any opt-out ever used" bit that would wrongly opt Codex
        // out too.
        config.last_no_codex = false;
        config.last_codex_telemetry_only = false;

        let optout = optout_from(&config);
        assert!(optout.claude, "no_claude must carry forward");
        assert!(optout.vscode, "no_vscode must carry forward");
        assert!(!optout.codex, "no_codex was never set; must stay false");
        assert!(
            !optout.codex_telemetry_only,
            "codex_telemetry_only was never set; must stay false"
        );
    }

    /// A machine that never ran `configure`/`login` on a build with this
    /// field reads all four as `false` -- the same value
    /// `ClientOptOut::default()` always was, so this changes nothing for a
    /// developer who never used an opt-out flag.
    #[test]
    fn optout_from_config_defaults_to_opting_out_of_nothing() {
        let optout = optout_from(&test_config());
        let default = ClientOptOut::default();
        assert_eq!(optout.claude, default.claude);
        assert_eq!(optout.codex, default.codex);
        assert_eq!(optout.vscode, default.vscode);
        assert_eq!(optout.codex_telemetry_only, default.codex_telemetry_only);
    }

    fn test_config() -> OauthConfig {
        OauthConfig {
            issuer: "https://issuer.example".to_owned(),
            client_id: "cli".to_owned(),
            scopes: "openid".to_owned(),
            audience: None,
            otel_endpoint: None,
            otel_token: None,
            gateway_url: None,
            profile: crate::profile::Profile::Manual,
            profile_explicit: None,
            copilot_spool_path: None,
            otel_headers_debounce_ms: 240_000,
            open_browser: false,
            token_exchange: None,
            last_no_claude: false,
            last_no_codex: false,
            last_no_vscode: false,
            last_codex_telemetry_only: false,
        }
    }
}
