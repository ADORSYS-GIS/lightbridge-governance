//! The persistent record of what this binary did, and why it stopped.
//!
//! Before this module every diagnostic went to stderr and nowhere else.
//! That is fine for `login`, which a human is watching. It is useless for
//! the callers that matter most: `token`/`otel-headers`, spawned every few
//! minutes by Claude Code and Codex with their stderr swallowed, and
//! `copilot-push`, woken by a timer at 03:00 with nobody there at all. A
//! drain that failed on that schedule left nothing to read afterwards.
//!
//! ## Where the file lives
//!
//! ADR-0012 §1 fixes a location per KIND of data, not per platform, and
//! logs are their own kind -- neither the session (state we must not lose)
//! nor the discovery document (cache the OS may purge at will):
//!
//! | Linux | macOS |
//! |---|---|
//! | `$XDG_STATE_HOME/governance-auth/logs/`, else `~/.local/state/…` | `~/Library/Logs/governance-auth/` |
//!
//! Linux is the XDG basedir spec taken literally -- it names "actions
//! history (logs, …)" as an example of `$XDG_STATE_HOME`'s contents -- so
//! this is [`crate::cache::state_dir`] plus one segment, and it inherits
//! that directory's `0700`.
//!
//! macOS is `~/Library/Logs`: Apple's per-user log location, what
//! Console.app reads, and -- decisively -- where the launchd agent this
//! binary installs ALREADY redirects the drain's stderr. Adding a second
//! log elsewhere would leave whoever debugs a 03:00 failure with two files
//! and no way to tell which is authoritative, so the agent was moved onto
//! this exact path instead ([`path_in`], `crate::schedule::launchd`). One
//! file, two writers; [`rotate`] is built for precisely that.
//!
//! ## What must never be in it
//!
//! A token on stderr is gone once the terminal scrolls; a token in a file
//! is a credential at rest. Nothing is logged that was not already safe to
//! print to stderr, secrets travel in [`crate::redacted::Redacted`] (whose
//! `Debug` is `<redacted>` and which has no `Display` at all), and
//! `tests/logging_redaction.rs` runs the real binary at `trace` with a
//! sentinel token and greps the resulting file for it.
//!
//! stdout is never a sink here: one layer is pinned to stderr, the other to
//! the file. `token`'s stdout carries the access token and nothing else.

mod rotate;
#[cfg(test)]
mod tests;
mod writer;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Named for the binary, not for `copilot-push`: every command writes here.
const FILE_NAME: &str = "governance-auth.log";

/// How loud the FILE is, independent of `RUST_LOG`. It exists to be read
/// after the fact by someone who could not have set an env var at the time,
/// so it defaults to `info` rather than to off.
const LEVEL_ENV: &str = "GOVERNANCE_AUTH_LOG";

/// macOS's log path for a given `$HOME`. Pure and unconditional so
/// `schedule::launchd` -- whose rendering tests run on Linux CI -- can point
/// the agent's `StandardErrorPath` at the same file this module opens.
pub(crate) fn path_in(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Logs")
        .join("governance-auth")
        .join(FILE_NAME)
}

fn path() -> Result<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Ok(crate::cache::state_dir()?.join("logs").join(FILE_NAME));
    }
    let home = std::env::var("HOME").context("locating the log directory (HOME unset)")?;
    Ok(path_in(Path::new(&home)))
}

/// Installs the subscriber. Infallible by construction: a machine where the
/// log file cannot be opened (read-only `$HOME`, full disk) still gets the
/// stderr layer and still authenticates -- losing the record is a degraded
/// install, refusing to mint a token over it would be an outage.
pub fn init() {
    let file = match path().and_then(|path| writer::open(&path)) {
        Ok(handle) => Some(
            fmt::layer()
                // No colour escapes in a file someone will `grep`.
                .with_ansi(false)
                .with_writer(handle)
                .with_filter(
                    EnvFilter::builder()
                        .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                        .parse_lossy(std::env::var(LEVEL_ENV).unwrap_or_default()),
                ),
        ),
        Err(error) => {
            // stderr, never stdout, and only when logging is actually
            // broken -- silence here would hide the one failure that makes
            // every later diagnosis impossible.
            eprintln!("warning: file logging is disabled: {error:#}");
            None
        }
    };

    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(EnvFilter::from_default_env()),
        )
        .with(file)
        .init();
}

/// Records how a command ended and hands the outcome straight back, so the
/// caller's control flow is unchanged. `{error:#}` is the whole `anyhow`
/// context chain -- the same text `main`'s `Result` already prints to stderr
/// on exit, so nothing new moves into the file; it just survives.
pub fn finish(outcome: Result<()>) -> Result<()> {
    match &outcome {
        Ok(()) => tracing::info!("command completed"),
        Err(error) => tracing::error!(cause = format!("{error:#}"), "command failed"),
    }
    outcome
}
