//! Which clients a run may touch -- and why "skip" is not "do not write".
//!
//! `--no-claude`, `--no-codex` and `--no-vscode` each say: leave that client
//! exactly as it is. The developer who needs this is the one who manages one
//! of the three by hand -- a `settings.json` shared through a dotfiles repo, a
//! `config.toml` whose provider block they wrote themselves -- and who still
//! wants the other two wired by this binary.
//!
//! ## Skipping the write is only half of the job
//!
//! [`crate::otel::configure_all`] records the keys it wrote, and
//! [`crate::managed::retract_stale`] removes anything recorded last time that
//! this run did not write again. That retraction is not incidental: it is how
//! the VS Code exporter cutover took `otlpEndpoint` back out of every machine
//! the old build had configured.
//!
//! So a client skipped only at the WRITE drops out of that record, and the very
//! next thing `configure` does is conclude we no longer manage its keys and
//! **delete them from the developer's file** -- the exact opposite of what the
//! flag promises, and not recoverable from. An opted-out client is therefore
//! excluded from the retraction computation as well, with its previous manifest
//! entry carried forward untouched so that a later run *without* the flag still
//! knows which keys are ours. [`crate::managed::plan`] is the single place
//! those two halves meet, and the only way to build that record.
//!
//! ## `--no-vscode` leaves the drain schedule exactly as it is
//!
//! The five-minute `copilot push` timer ([`crate::schedule`]) exists for one
//! reason: Copilot Chat's file exporter appends to a spool and nothing in VS
//! Code ships it. So the timer is VS Code's, and the flag reaches it -- but it
//! neither installs one nor removes one.
//!
//! Removing it would be the retraction trap wearing a different hat. This flag
//! deliberately does not turn Copilot's exporter off, so a machine an earlier
//! run configured is *still writing that spool*; tearing the timer down would
//! leave it growing for ever and drained by nothing, which is the failure
//! [`crate::schedule`] was added to remove. Installing one is no better on a
//! machine that has never had VS Code configured: a wake every five minutes,
//! spending a token refresh to drain a file nothing writes. Leaving it alone is
//! the only reading of "leave that client alone" that is true in both
//! directions.
//!
//! ## What these flags do not reach
//!
//! The shell rc block and `~/.config/governance-auth/otel.env` are the
//! developer's shell, not a client's config file, and this binary reads them
//! itself. They are written regardless -- which is why all three flags together
//! are a notice and not an error: `configure` still has real work to do, unlike
//! the neither-`--otel-endpoint`-nor-`--gateway-url` case, where there is no
//! configuration to compute at all.

use std::path::Path;

use clap::Args;

use crate::config::OauthConfig;

#[cfg(test)]
mod tests;

/// Clients this run must not touch.
#[derive(Debug, Clone, Copy, Default, Args)]
pub struct ClientOptOut {
    /// Leave Claude Code's `~/.claude/settings.json` exactly as it is.
    #[arg(long = "no-claude")]
    pub claude: bool,
    /// Leave Codex's `~/.codex/config.toml` exactly as it is.
    #[arg(long = "no-codex")]
    pub codex: bool,
    /// Leave VS Code's `settings.json` and the Copilot drain schedule alone.
    #[arg(long = "no-vscode")]
    pub vscode: bool,
}

impl ClientOptOut {
    /// Installs or removes the Copilot drain schedule -- unless VS Code was
    /// opted out, in which case it is left exactly as it is.
    ///
    /// Non-fatal, like [`crate::schedule`] itself: every config file is
    /// already written by the time this runs, and a machine with no user
    /// systemd session must not turn a successful `configure` into a failure.
    pub fn apply_schedule(self, home: &Path, config: &OauthConfig) {
        if self.vscode {
            eprintln!(
                "Left alone (--no-vscode): the Copilot drain schedule -- neither installed nor \
                 removed."
            );
            return;
        }
        if let Err(error) = crate::schedule::apply(home, config) {
            eprintln!("warning: could not update the Copilot drain schedule: {error:#}");
        }
    }

    /// Says out loud that nothing was written for any client, when that is
    /// what was asked for. Silence here would read exactly like the silent
    /// no-op `configure` used to perform with neither endpoint supplied, which
    /// is the confusion that made that case a hard error.
    pub fn report_if_every_client_declined(self) {
        if !(self.claude && self.codex && self.vscode) {
            return;
        }
        eprintln!(
            "All three clients were opted out, so no client configuration was written and the \
             Copilot drain schedule was left as it is. The shell environment file and this \
             binary's own settings were still updated."
        );
    }
}
