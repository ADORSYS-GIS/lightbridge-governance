//! The command tree, and the one rule that decided its shape.
//!
//! ## Why these three scopes and not others
//!
//! The old surface was a flat list of eight, three of which were a scope and a
//! verb hyphenated into one token: `copilot-push`, `otel-headers`,
//! `self-update`. Every hyphen in that list was a namespace the CLI declined
//! to admit it had, so the rule is simply: **a hyphenated compound becomes a
//! real scope, and a single-word verb stays top-level.** That gives
//! `copilot push`, `otel headers` and `self update`, and leaves `login`,
//! `logout`, `refresh`, `status`, `configure` and `token` where they are --
//! six verbs on the one object this binary owns, which is a session. A
//! `session` scope wrapping those six would be a level of tree that answers no
//! question a reader has.
//!
//! ## The rule that decided which names were allowed to move
//!
//! Renaming a command breaks every file that already embeds the old one, and
//! those files are not ours:
//!
//! **A command name may move only if `configure` owns every file that embeds
//! it.**
//!
//! - `otel-headers` appears in Claude Code's `otelHeadersHelper`, which
//!   `configure` rewrites -> free to move.
//! - `copilot-push` appears in the systemd unit / launchd plist, which
//!   `configure` rewrites ([`crate::schedule`]) -> free to move.
//! - `self-update` appears in no file at all -> free to move.
//! - `token` appears in `apiKeyHelper` and Codex's `auth.command` (both ours)
//!   **and in the VS Code extension's `execFile` argv**, which is a different
//!   product on a different release train. `configure` cannot reach it, so
//!   `token` is FROZEN -- top-level, unchanged, and it stays a bare word.
//!
//! There are no aliases for the old names. The two that break are both files
//! `configure` rewrites, and [`crate::dashboard`] compares what is on disk
//! against what [`invoke`] generates now, so `status` names the fix rather
//! than leaving a developer who only ran `self update` to find it.
//!
//! `cli::tests::accepts` walks this tree by name; it is what lets [`invoke`]
//! assert that the command lines it writes into other tools' files are ones
//! this binary still answers to.

mod invoke;
mod scopes;
mod verbs;

use anyhow::Result;
use clap::Parser;
pub use invoke::{
    COPILOT_PUSH, OTEL_HEADERS_TAIL, TOKEN_TAIL, otel_headers_command, token_command,
};
use scopes::{CopilotCommand, OtelCommand, SelfCommand};
use verbs::Command;

use crate::{config::OauthConfigArgs, copilot, dashboard, oauth, update};

#[derive(Debug, Parser)]
#[command(
    name = "governance-auth",
    // Not bare `version` (which clap wires to `CARGO_PKG_VERSION`): on a
    // released binary that is the stale workspace version, so `--version` and
    // the version `self update` acts on would disagree -- and `--version` is
    // exactly what someone runs to check whether an update landed. Same source
    // for both. See `update::VERSION`.
    version = update::VERSION,
    // A bare invocation is someone looking for the tree, not someone who made
    // a mistake: show it instead of a one-line usage error.
    arg_required_else_help = true,
    about = "OAuth2 credential helper for pointing Claude Code / Codex / VS Code Copilot at this \
             org's OIDC-backed gateway.",
    // ⚠️ Set explicitly, and it has to be. With only `about`, clap derive takes
    // the LONG help from the flattened `OauthConfigArgs`, so `--help` opened
    // with two paragraphs of that struct's internal rationale about
    // `global = true` and clap's `Option` handling -- accurate, addressed to a
    // maintainer, and the first thing a new developer read.
    long_about = "OAuth2 credential helper for pointing Claude Code / Codex / VS Code Copilot \
                  at this org's OIDC-backed gateway.\n\n\
                  `login` runs the interactive flow once. `token` then prints a valid access \
                  token on every later call, and is what Claude Code's `apiKeyHelper` and \
                  Codex's `auth.command` are wired to. `configure` writes that wiring, the OTLP \
                  export config, and the schedule that drains VS Code Copilot's telemetry \
                  spool.\n\n\
                  Every configuration option is accepted BEFORE or AFTER the command, so one \
                  command line can be embedded in another tool's config file.",
    after_help = "Full option matrix, precedence and config-file format: \
                  docs/governance-auth/configuration.md"
)]
pub struct Cli {
    #[command(flatten)]
    oauth: OauthConfigArgs,

    #[command(subcommand)]
    command: Command,
}

impl Cli {
    /// Resolves the configuration a command needs and runs it.
    ///
    /// ⚠️ Resolved PER COMMAND, not once up front. `self update` talks only to
    /// the GitHub releases API and reads none of it -- resolving before the
    /// dispatch made it fail with `--issuer (or GOVERNANCE_AUTH_ISSUER) is
    /// required` on a machine that had no config yet, which is exactly the
    /// machine most likely to be updating. Every other command still resolves
    /// before it does any work, so a missing value is still reported
    /// immediately with no partial work done.
    pub async fn run(self, http: &reqwest::Client) -> Result<()> {
        tracing::info!(command = ?self.command, version = update::VERSION, "invoked");
        match self.command {
            Command::Login {
                device_code,
                optout,
            } => oauth::login(http, &self.oauth.resolve()?, device_code, optout).await,
            Command::Token => oauth::token(http, &self.oauth.resolve()?).await,
            Command::Refresh => oauth::refresh(http, &self.oauth.resolve()?).await,
            Command::Status => dashboard::status(&self.oauth.resolve()?),
            Command::Configure { optout } => oauth::configure(&self.oauth.resolve()?, optout),
            Command::Logout => oauth::logout(http, &self.oauth.resolve()?).await,
            Command::Otel {
                command: OtelCommand::Headers,
            } => oauth::otel_headers(http, &self.oauth.resolve()?).await,
            Command::Copilot {
                command: CopilotCommand::Push { dry_run },
            } => copilot::run(http, &self.oauth.resolve()?, dry_run).await,
            // Deliberately does NOT resolve: see the doc above.
            Command::Own {
                command: SelfCommand::Update { dry_run },
            } => update::run(http, dry_run).await,
        }
    }
}

#[cfg(test)]
mod tests;
