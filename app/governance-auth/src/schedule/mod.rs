//! The drain's schedule: the half of the Copilot telemetry path that VS Code
//! does not run for you.
//!
//! Copilot Chat's file exporter appends records to a spool and stops there.
//! Nothing in VS Code ships them. [`crate::copilot`] is that something else,
//! and until this module existed, installing the timer that ran it was a
//! section of the runbook -- which is not a default. The measurable outcome of
//! leaving it to the reader is a machine that spools telemetry to disk for
//! ever and exports none of it, and from inside the editor that looks exactly
//! like a working install.
//!
//! ## This reverses an explicit earlier decision, on purpose
//!
//! `docs/governance-auth/commands.md` used to say this binary does not install
//! these units "deliberately -- writing to a developer's systemd or launchd
//! tree is a bigger claim on their machine than writing a dotfile". The claim
//! is real and it is now made anyway: `configure` already writes four config
//! files across three tools, and the one step it left to the human was the one
//! without which none of the others do anything for Copilot.
//!
//! ## Everything here is non-fatal
//!
//! A machine with no user systemd session -- a container, a WSL install
//! without systemd, a CI runner -- must not turn a successful `configure` into
//! a failed one. The config files are already written by the time this runs
//! and `copilot push` still works by hand, so a failure is reported with the
//! two commands that finish the job and swallowed by the caller.
//!
//! ## Why the unit carries flags instead of trusting the config file
//!
//! `configure` writes `issuer`/`client_id`/`otel_endpoint`/
//! `copilot_spool_path` to the per-user config file, so a bare
//! `governance-auth copilot push` would resolve today. It is passed
//! explicitly anyway: only the explicit form keeps working after someone edits
//! that file, and a wake that fails every five minutes because a key moved is
//! precisely the silent failure this module exists to remove.

pub mod daemon;
mod launchd;
mod staleness;
mod systemd;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
pub use staleness::stale;

use crate::{cli, config::OauthConfig};

/// How often the drain wakes.
///
/// Five minutes. The spool was measured growing 73 KB -> 315 KB in six minutes
/// of ordinary use (RFC-0003 §2a), so a longer interval buys nothing but disk
/// and a wider loss window, and a shorter one spends a token refresh per wake
/// on records that have not been written yet.
pub const INTERVAL_SECONDS: u64 = 300;

/// launchd's reverse-DNS job label.
pub const LABEL: &str = "digital.camer.ai.governance-auth.copilot-push";

/// systemd's unit stem -- `<UNIT>.service` is run by `<UNIT>.timer`.
pub const UNIT: &str = "governance-auth-copilot-push";

/// Exactly what the scheduler runs, resolved once at `configure` time.
#[derive(Debug, Clone)]
pub struct Invocation {
    /// Absolute path to this binary. A scheduler does not inherit a login
    /// shell's `PATH`, so a bare name fails on every wake -- the same trap
    /// [`crate::otel::OtelSettings::token_command`] documents for Codex.
    pub program: String,
    /// Everything after it, in clap's order: global flags, then the
    /// subcommand. `tests/cli_arg_order.rs` is what pins that order.
    pub args: Vec<String>,
}

impl Invocation {
    /// `None` when no collector is configured, or under the `daemon`
    /// profile (ADR-0016 / #270 AC5): the daemon forwards Copilot's spool
    /// itself once #272 rewires its exporter, so a `manual`-only timer
    /// draining the same file would double-export. Either way there is
    /// nothing for this timer to do, so it is removed rather than installed
    /// pointing at nothing -- the same rule that already applied to a
    /// missing endpoint, now also applied to the profile that owns this
    /// path.
    ///
    /// No `serve_otel_is_supported()` check here either (#280 review round
    /// 2, same reasoning as [`crate::oauth`]'s `TelemetryWiring::resolve`):
    /// `apply_telemetry`'s chokepoint has already refused the call entirely
    /// when `profile != Manual` on a build that can't serve `daemon`, so by
    /// the time this sees a non-`Manual` profile it is safe to remove the
    /// timer -- the daemon that replaces it is guaranteed to actually exist.
    fn resolve(config: &OauthConfig) -> Result<Option<Self>> {
        if config.profile != crate::profile::Profile::Manual {
            return Ok(None);
        }
        let Some(endpoint) = config.otel_endpoint.as_deref() else {
            return Ok(None);
        };
        let spool = crate::copilot::resolve_spool_path(config)?;
        Ok(Some(Self {
            program: crate::otel::binary_path(),
            args: vec![
                "--issuer".to_owned(),
                config.issuer.clone(),
                "--client-id".to_owned(),
                config.client_id.clone(),
                "--otel-endpoint".to_owned(),
                endpoint.to_owned(),
                "--copilot-spool-path".to_owned(),
                spool.to_string_lossy().into_owned(),
            ]
            .into_iter()
            .chain(cli::COPILOT_PUSH.iter().map(|word| (*word).to_owned()))
            .collect(),
        }))
    }
}

/// Runs a scheduler command to completion, folding its stderr into the error.
///
/// `output()`, not `status()`: `systemctl` and `launchctl` both explain
/// themselves on stderr, and letting that stream straight through would
/// interleave their diagnostics with `configure`'s own report while throwing
/// away the one line the caller needs to quote.
fn run(program: &str, args: &[&str]) -> Result<()> {
    let rendered = format!("{program} {}", args.join(" "));
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running `{rendered}`"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("`{rendered}` failed: {}", stderr.trim());
}

/// Whether this platform's scheduler is launchd. A `cfg!` rather than a
/// `#[cfg]` on the modules themselves: both compile everywhere, so the
/// renderers are unit-testable on either host and a plist bug cannot hide
/// until a macOS-only CI job runs.
fn macos() -> bool {
    cfg!(target_os = "macos")
}

/// Installs the drain schedule, or removes it when no collector is
/// configured. Reports what it did on stderr; returns `Err` only for a
/// failure the caller should surface as a warning.
pub fn apply(home: &Path, config: &OauthConfig) -> Result<()> {
    match (Invocation::resolve(config)?, macos()) {
        (Some(invocation), true) => launchd::install(home, &invocation),
        (Some(invocation), false) => systemd::install(home, &invocation),
        (None, true) => launchd::remove(home),
        (None, false) => systemd::remove(home),
    }
}

/// What `status` reports. Deliberately three-valued: a scheduler that could
/// not be asked is `None`, never `Some(false)` -- claiming a drain is stopped
/// when the question was never answered is the same class of error as
/// claiming it is running.
pub struct Schedule {
    /// The unit or plist this platform would have written.
    pub path: PathBuf,
    /// `true` when that file is on disk.
    pub installed: bool,
    /// `true` when the platform's scheduler confirms it is loaded/active,
    /// `false` when it confirms it is not, `None` when it could not be asked.
    pub active: Option<bool>,
}

/// Reads one file and runs one short local command. No network, matching the
/// rest of `status` -- see [`crate::dashboard::spool`].
pub fn survey(home: &Path) -> Schedule {
    if macos() {
        launchd::survey(home)
    } else {
        systemd::survey(home)
    }
}

/// The command that starts a schedule which is installed but not running,
/// for this platform. Lives here rather than in the dashboard so `status` has
/// no reason to branch on the operating system.
pub fn start_command() -> String {
    if macos() {
        format!("launchctl kickstart -k gui/$(id -u)/{LABEL}")
    } else {
        format!("systemctl --user enable --now {UNIT}.timer")
    }
}
