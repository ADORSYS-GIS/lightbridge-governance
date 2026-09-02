//! The unscoped verbs: the six that act on a session, plus the three that open
//! a scope.
//!
//! Split from [`super`] for the same reason [`super::scopes`] is: no file here
//! has to choose between fitting the 200-line ceiling and carrying the help
//! text that makes `--help` worth reading. *Which* names are top-level, which
//! are a scope, and which of them were allowed to move at all, stays in
//! [`super`]'s module doc -- next to the decision, not next to the enum.

use clap::Subcommand;

use super::scopes::{CopilotCommand, OtelCommand, SelfCommand};
use crate::optout::ClientOptOut;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the interactive login once and cache the session.
    ///
    /// Prints an authorize URL to visit -- or, with `--device-code`, a
    /// verification URL and a code, then polls. Does NOT open a browser by
    /// default; see `--open-browser`.
    Login {
        /// Use the device-authorization flow, for a headless session with
        /// no local browser.
        ///
        /// Independent of `--open-browser`, which only affects the loopback
        /// flow: there is nothing to open a browser to here, because the
        /// verification URL is meant for a different device.
        #[arg(long)]
        device_code: bool,
        #[command(flatten)]
        optout: ClientOptOut,
    },
    /// Print a currently-valid access token to stdout.
    ///
    /// Nothing else goes on stdout, ever. This is the command to wire into
    /// `apiKeyHelper` / `auth.command`. Fails closed and non-interactively
    /// when there is no valid session.
    Token,
    /// Force a refresh now, even when the cached session is still fresh.
    ///
    /// `token` only refreshes inside the expiry skew, which is right for a
    /// helper spawned every few minutes and useless when the reason you want
    /// a new token is a change at the server. Prints nothing on stdout and
    /// never logs in interactively: it renews a session or it fails.
    Refresh,
    /// Report the session, the telemetry wiring and the Copilot drain.
    ///
    /// Prints whether a cached session exists and how fresh it is, and
    /// whether the wiring `configure` wrote is still the wiring this binary
    /// generates today.
    Status,
    /// Re-apply the tool configuration and the drain schedule.
    ///
    /// Rewrites the Claude Code / Codex / VS Code wiring without re-running
    /// the interactive login.
    ///
    /// `login` already does this. Run it for an existing session whose
    /// endpoint or ingest token changed, after installing one of those tools
    /// for the first time, and after upgrading this binary -- an upgrade can
    /// change the commands written into their config.
    ///
    /// `--no-claude` / `--no-codex` / `--no-vscode` each leave one client
    /// entirely alone: not written, and not retracted either.
    Configure {
        #[command(flatten)]
        optout: ClientOptOut,
    },
    /// Remove the cached session, revoking the refresh token first.
    Logout,
    /// OTLP export helpers.
    Otel {
        #[command(subcommand)]
        command: OtelCommand,
    },
    /// The VS Code Copilot Chat telemetry path.
    Copilot {
        #[command(subcommand)]
        command: CopilotCommand,
    },
    /// This binary, acting on itself.
    #[command(name = "self")]
    Own {
        #[command(subcommand)]
        command: SelfCommand,
    },
}
