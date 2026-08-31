//! The `status` subcommand itself.
//!
//! Moved here from `crate::oauth` when the Copilot spool row was added: it
//! surveys four sources (the session cache, the managed-key manifest, the
//! per-target config files and now the drain checkpoint) and renders them,
//! which is this module's job and not the OAuth flow's. `oauth` keeps the
//! commands that actually talk to the authorization server.
//!
//! The TTY split is the contract described in [`super`]'s module doc: with no
//! terminal, `status` prints exactly the one documented line it always has,
//! because Claude Code and Codex parse this binary's output.

use anyhow::Result;

use super::{Session, Spool, Telemetry, attended, plain, render, targets};
use crate::{cache, config::OauthConfig};

pub fn status(config: &OauthConfig) -> Result<()> {
    let state = match cache::load(&config.issuer, &config.client_id)? {
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
    };

    // Plain line unless a human is looking. The three strings below are a
    // documented surface (`commands.md`) that a test asserts on, and `status`
    // may be piped; the table is an addition, never a replacement.
    if !attended() {
        eprintln!("{}", plain(&state));
        return Ok(());
    }

    let home = std::env::var("HOME")
        .ok()
        .filter(|home| !home.is_empty())
        .map(std::path::PathBuf::from);
    let target_rows = home.as_deref().map(targets).unwrap_or_default();
    // Endpoint from the resolved config (it is persisted); everything else from
    // what was actually written -- see `super::telemetry`'s module doc for why
    // the token cannot be read back off the config.
    let telemetry = Telemetry::survey(home.as_deref(), config.otel_endpoint.clone());
    // Reads two local files and never the network, same as the rest of this
    // command -- see `super::spool`.
    let spool = Spool::survey(config);
    eprintln!(
        "{}",
        render(
            &config.issuer,
            &config.client_id,
            &state,
            &telemetry,
            &spool,
            &target_rows,
        )
    );
    Ok(())
}
