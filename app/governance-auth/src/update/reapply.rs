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
    match oauth::configure(&config, ClientOptOut::default()) {
        Ok(()) => eprintln!("Re-applied `configure` for the new version."),
        Err(error) => eprintln!(
            "warning: updated the binary, but could not re-apply configuration ({error:#}) -- \
             run `governance-auth configure` by hand."
        ),
    }
}
