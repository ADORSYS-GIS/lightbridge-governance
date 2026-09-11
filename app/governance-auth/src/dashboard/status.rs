//! The `status` subcommand itself.
//!
//! Moved here from `crate::oauth` when the Copilot spool row was added: it
//! surveys five sources (the session cache, the managed-key manifest, the
//! per-target config files, the drain checkpoint, and now the daemon
//! service) and renders them, which is this module's job and not the OAuth
//! flow's. `oauth` keeps the commands that actually talk to the
//! authorization server.
//!
//! The TTY split is the contract described in [`super`]'s module doc: with no
//! terminal and no `--json`, `status` prints exactly the one documented line
//! it always has, because Claude Code and Codex parse this binary's output.
//!
//! ## `--json` bypasses the TTY gate on purpose
//!
//! The gate exists so a table meant for a human never lands in a pipe a tool
//! parses. `--json` is the opposite case: an explicit ask, from a script, CI,
//! or an agent, for the same five rows a human would see -- so it always
//! gathers all five surveys and always prints them, whether or not a terminal
//! is attached. It writes to **stdout**, not stderr: nothing on stdout is a
//! documented contract for `status` today (unlike the plain line, which is),
//! so this claims the stream clean rather than competing with it.

use anyhow::{Context, Result};

use super::{
    Daemon, Drain, OtelSpool, Session, Spool, Surveys, Target, Telemetry, attended, plain, render,
    render_json, rows_plain, targets,
};
use crate::{cache, config::OauthConfig};

/// The session state every rendering of `status` starts from -- split out so
/// [`survey_rows`] (for `doctor`) shares it with [`status`] itself rather
/// than loading the cache a second, possibly different, way.
fn load_session(config: &OauthConfig) -> Result<Session> {
    Ok(match cache::load(&config.issuer, &config.client_id)? {
        Some(session) => Session {
            cached: true,
            fresh: session.is_fresh()?,
            expires_in: session.seconds_until_expiry()?,
        },
        None => Session {
            cached: false,
            fresh: false,
            expires_in: 0,
        },
    })
}

pub fn status(config: &OauthConfig, json: bool) -> Result<()> {
    let state = load_session(config)?;

    if json {
        let (target_rows, telemetry, daemon, otel_spool, spool, drain) = gather(config);
        let out = render_json(
            &config.issuer,
            &config.client_id,
            &state,
            &Surveys {
                telemetry: &telemetry,
                daemon: &daemon,
                otel_spool: &otel_spool,
                spool: &spool,
                drain: &drain,
            },
            &target_rows,
        )
        .context("rendering status as JSON")?;
        println!("{out}");
        return Ok(());
    }

    // Plain line unless a human is looking. The three strings below are a
    // documented surface (`commands.md`) that a test asserts on, and `status`
    // may be piped; the table is an addition, never a replacement.
    if !attended() {
        eprintln!("{}", plain(&state));
        return Ok(());
    }

    let (target_rows, telemetry, daemon, otel_spool, spool, drain) = gather(config);
    eprintln!(
        "{}",
        render(
            &config.issuer,
            &config.client_id,
            &state,
            &Surveys {
                telemetry: &telemetry,
                daemon: &daemon,
                otel_spool: &otel_spool,
                spool: &spool,
                drain: &drain,
            },
            &target_rows,
        )
    );
    Ok(())
}

/// The five per-source surveys plus the per-target rows, gathered once so the
/// table path and the `--json` path can never read a different set of files
/// for what is supposed to be the same answer.
fn gather(config: &OauthConfig) -> (Vec<Target>, Telemetry, Daemon, OtelSpool, Spool, Drain) {
    let home = std::env::var("HOME")
        .ok()
        .filter(|home| !home.is_empty())
        .map(std::path::PathBuf::from);
    let target_rows = home.as_deref().map(targets).unwrap_or_default();
    // Endpoint from the resolved config (it is persisted); everything else from
    // what was actually written -- see `super::telemetry`'s module doc for why
    // the token cannot be read back off the config.
    let telemetry = Telemetry::survey(home.as_deref(), config);
    // Reads the unit/plist and asks the platform's scheduler whether it is
    // loaded, same as `drain` below -- bounded to `schedule::daemon::
    // ASK_TIMEOUT` rather than genuinely unbounded, since this is `status`'s
    // SECOND such shell-out per run and a hung one must not hang the whole
    // command. See `super::daemon`.
    let daemon = Daemon::survey(home.as_deref(), config);
    // Reads this daemon's own spool file and checkpoint, never the network --
    // see `super::otel_spool`.
    let otel_spool = OtelSpool::survey(config);
    // Reads two local files and never the network, same as the rest of this
    // command -- see `super::spool`.
    let spool = Spool::survey(config);
    // Reads the unit/plist and asks the platform's scheduler whether it is
    // loaded -- one short local command, no network. See `super::drain`.
    let drain = Drain::survey(home.as_deref(), config);
    (target_rows, telemetry, daemon, otel_spool, spool, drain)
}

/// Every row `status --json` would show, as plain owned data -- for
/// `doctor` (see its own module doc), which needs a verdict per row without
/// going through a JSON string or touching this module's private `Colour`.
/// Shares [`load_session`] and [`gather`] with `status` itself, so the two
/// can never disagree about what a fresh look at this machine finds.
pub fn survey_rows(config: &OauthConfig) -> Result<Vec<(String, String, String, String)>> {
    let state = load_session(config)?;
    let (target_rows, telemetry, daemon, otel_spool, spool, drain) = gather(config);
    Ok(rows_plain(
        &config.issuer,
        &config.client_id,
        &state,
        &Surveys {
            telemetry: &telemetry,
            daemon: &daemon,
            otel_spool: &otel_spool,
            spool: &spool,
            drain: &drain,
        },
        &target_rows,
    ))
}
