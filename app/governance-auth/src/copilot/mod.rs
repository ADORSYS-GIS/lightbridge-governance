//! `copilot push`: drains VS Code Copilot Chat's OTel spool file and exports
//! it to the governed collector over OTLP/HTTP.
//!
//! Copilot Chat is told to write telemetry to a file
//! (`github.copilot.chat.otel.exporterType = "file"`, `outfile = <path>`) by
//! `configure` -- see [`crate::vscode`] for why that exporter and not the
//! direct HTTP one. Nothing in VS Code then drains that file. This does, on
//! the schedule [`crate::schedule`] installs: a systemd user timer on Linux, a
//! launchd agent on macOS, every five minutes.
//!
//! Both halves are `configure`'s job now. They used to be two paragraphs of a
//! runbook, which meant the endpoint, the spool path and the timer were three
//! copy-pastes that could disagree -- and a machine where they did looked, from
//! inside VS Code, exactly like one where they did not.
//!
//! ## The one property that matters: fail closed
//!
//! **A run that cannot produce a valid bearer must not consume data.** The
//! bearer is obtained *first*, before the spool is opened, before the
//! checkpoint is read, and unconditionally -- `--dry-run` included. That
//! ordering, not a flag check, is what makes "auth failed" and "data
//! discarded" unreachable together: there is no code path from
//! [`crate::oauth::current_session`] failing to a `checkpoint::store` call,
//! because the `?` returns before either exists.
//!
//! `--dry-run` is deliberately held to the same bar rather than being an
//! offline escape hatch. An offline preview mode would be a second path that
//! reads the spool without a token, and "there is exactly one such path and it
//! starts with authentication" is a far easier property to keep true than "all
//! the paths that read the spool are safe for their own reasons".
//!
//! ## And the second: nothing is lost quietly
//!
//! An offset advances only over records that were delivered *or* recorded as
//! lost. Those are the two outcomes; there is no third one where bytes go past
//! the checkpoint unaccounted for. The reason it is stated as "or recorded"
//! rather than "only when delivered" is that pure refusal-to-advance is not
//! safe either: one record the collector will never take would stop the stream
//! at that byte offset for good, and every record written after it with it. So
//! the drain is allowed to give up -- and every time it does, the count and
//! the time land in the checkpoint and `status` stops being green. See
//! [`export`] for the rule that keeps a misconfigured collector from using
//! that permission to empty the whole spool.
//!
//! ## And the third: the file does not grow for ever
//!
//! The spool used to be read-only to this binary, on the stated grounds that
//! truncating a file VS Code holds open leaves a zero-filled hole. That was
//! measured false on 2026-09-02 -- Copilot's descriptors are `O_APPEND`, so an
//! append after a truncate lands at byte 0 -- and the file had reached 12 MB
//! on the machine that measured it. [`spool::reclaim`] now truncates it, but
//! only when `size == offset` exactly, so it destroys nothing the checkpoint
//! has not already passed. That precondition narrows rather than closes the
//! race with a concurrent append; the module doc states its measured bound
//! instead of pretending it is zero.

mod batch;
mod checkpoint;
mod classify;
mod drain;
mod export;
mod journal;
mod lock;
mod logs;
mod metrics;
mod otlp;
mod pass;
mod points;
mod private_file;
mod push;
mod quarantine;
mod record;
mod spool;
mod status;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use anyhow::{Context, Result};
pub use status::SpoolStatus;

use crate::{config::OauthConfig, oauth};

/// Runs one drain.
///
/// `dry_run` still authenticates and still parses (so it reports exactly what
/// a real run would send), but posts nothing and moves nothing.
pub async fn run(http: &reqwest::Client, config: &OauthConfig, dry_run: bool) -> Result<()> {
    let endpoint = config.otel_endpoint.as_deref().context(
        "no collector configured: supply --otel-endpoint / GOVERNANCE_AUTH_OTEL_ENDPOINT (or set \
         `otel_endpoint` in your config file) before running `copilot push`",
    )?;
    let spool_path = resolve_spool_path(config)?;

    // ⚠️ FIRST, always. See the module doc: everything below this line
    // consumes data, and none of it may run without a valid credential.
    let session = oauth::current_session(http, config).await.context(
        "refusing to read the Copilot spool without a valid session -- nothing was consumed and \
         the checkpoint was not moved",
    )?;
    let bearer = oauth::emit_token(http, config, session).await.context(
        "refusing to read the Copilot spool without a valid bearer -- nothing was consumed and \
         the checkpoint was not moved",
    )?;

    drain::once(http, endpoint, &bearer, &spool_path, dry_run).await
}

/// ADR-0012 Decision 2's five layers for the spool path. The first four are
/// [`OauthConfig`]'s job (flag, env, per-user file, machine file); only the
/// compiled default is resolved here -- and it has to be, because it depends
/// on the state directory, which depends on `$HOME`. Handing clap a
/// `default_value` would fire before either config-file layer was consulted,
/// which is the trap `crate::config`'s module doc exists to warn about.
pub fn resolve_spool_path(config: &OauthConfig) -> Result<PathBuf> {
    match &config.copilot_spool_path {
        Some(path) => Ok(PathBuf::from(path)),
        None => Ok(crate::cache::state_dir()?.join(spool::DEFAULT_FILE_NAME)),
    }
}
