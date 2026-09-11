//! The persistent record of what this binary did, and why it stopped.
//!
//! Before this module every diagnostic went to stderr and nowhere else.
//! That is fine for `login`, which a human is watching. It is useless for
//! the callers that matter most: `token`/`otel headers`, spawned every few
//! minutes by Claude Code and Codex with their stderr swallowed, and
//! `copilot push`, woken by a timer at 03:00 with nobody there at all. A
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
//!
//! ## `GOVERNANCE_AUTH_LOG` only raises OUR level, never a dependency's
//!
//! `LEVEL_ENV` used to be handed to `EnvFilter::parse_lossy` verbatim. A bare
//! level word (`"trace"`, `"debug"` -- the only form ever documented, tested
//! (`tests/logging_redaction.rs`), or used in practice) is a directive with
//! no target, which `EnvFilter` applies as the DEFAULT for every target --
//! not just this crate's. `h2` depends on `tracing` unconditionally (not an
//! optional feature) and has 160 `trace!`/25 `debug!` call sites of its own;
//! measured with the exact `hyper`/`h2`/`reqwest` versions this daemon links
//! (2026-09-11, isolated harness, no TLS -- a real HTTPS hop only adds more):
//! **~40 KB of `h2`/`hyper`'s own output per request**, over a kept-alive
//! HTTP/2 connection, next to governance-auth's own single ~150-byte line
//! per request. Anyone who set `GOVERNANCE_AUTH_LOG=trace` to troubleshoot
//! this crate got every dependency's wire-level tracing for free, at roughly
//! 270x the byte cost -- a far larger amplifier of the incident
//! `otel_daemon::log_rotation`'s doc describes than request volume ever was.
//!
//! [`file_level`] resolves `LEVEL_ENV` to a bare [`tracing::level_filters::LevelFilter`]
//! (falling back to `info` on unset or unparseable, same as before) and
//! [`init`] scopes it to [`CRATE_TARGET`] alone, so no value anyone puts in
//! that variable can raise a dependency's own logging above the fixed `info`
//! default -- this is a narrowing, not a regression: nothing in this repo
//! ever set it to anything but a bare level.
//!
//! ## Rotation is a startup check, which only bounds a short process
//!
//! [`init`] rotates once, via [`writer::open`], before the file is ever
//! opened. For `token`/`otel headers`/`copilot push` that is enough: each is
//! a fresh process that re-execs within minutes and re-checks on its own, so
//! the live file is bounded "kilobytes, and bounded by the run" ([`rotate`]'s
//! own doc). `serve --otel` (`crate::otel_daemon`) is the one invocation that
//! is not a short run -- it is a `systemd`/`launchd` service meant to stay up
//! for the machine's entire uptime, and nothing after [`init`]'s one check
//! ever revisits the bound for it. [`recheck_rotation`] exists so that
//! process can ask again, on its own schedule, for as long as it runs --
//! see `crate::otel_daemon::log_rotation` for why that has to be its own
//! timer rather than piggybacked on the drain.

#[cfg(test)]
mod filter_tests;
mod rotate;
#[cfg(test)]
mod tests;
mod writer;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Named for the binary, not for `copilot push`: every command writes here.
const FILE_NAME: &str = "governance-auth.log";

/// How loud the FILE is, independent of `RUST_LOG`. It exists to be read
/// after the fact by someone who could not have set an env var at the time,
/// so it defaults to `info` rather than to off.
const LEVEL_ENV: &str = "GOVERNANCE_AUTH_LOG";

/// The only `tracing` target [`LEVEL_ENV`] is allowed to raise -- this
/// crate's own module path, never a dependency's. See this module's doc,
/// "`GOVERNANCE_AUTH_LOG` only raises OUR level".
const CRATE_TARGET: &str = "governance_auth";

/// Resolves [`LEVEL_ENV`] to a bare level, falling back to `info` when unset
/// or unparseable -- the same "loud enough to be useful, never silent"
/// default [`init`]'s own doc already commits to. Deliberately NOT
/// `EnvFilter::parse_lossy` on the raw string: that accepts a full
/// `target=level` directive DSL, and this only ever needs one level for one
/// target ([`CRATE_TARGET`]).
fn file_level() -> tracing::level_filters::LevelFilter {
    std::env::var(LEVEL_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(tracing::level_filters::LevelFilter::INFO)
}

/// Builds the file layer's filter for a given [`file_level`] result -- split
/// out from [`init`] purely so a test can drive it with an explicit level
/// instead of mutating the real process environment (racy across tests
/// running in parallel). Every OTHER target stays at the `with_default_directive`
/// below regardless of `level`; only [`CRATE_TARGET`] moves.
fn file_filter(level: tracing::level_filters::LevelFilter) -> EnvFilter {
    EnvFilter::builder()
        .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
        .parse_lossy(format!("{CRATE_TARGET}={level}"))
}

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
                .with_filter(file_filter(file_level())),
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

/// Re-checks the live file against [`rotate::MAX_BYTES`] and rotates if it
/// is still (or newly) oversized, independent of [`init`]'s one-time check.
///
/// Best-effort, exactly like [`rotate::maybe_rotate`] itself: an unresolvable
/// path (`HOME` unset, e.g.) or a rotation that cannot run leaves the file as
/// it was and returns silently. That is not a new failure mode -- it is the
/// same one [`init`]'s own startup check already accepts, asked again.
pub(crate) fn recheck_rotation() {
    if let Ok(path) = path() {
        rotate::maybe_rotate(&path);
    }
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
