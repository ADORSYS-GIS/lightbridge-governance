//! `copilot-push`: drains VS Code Copilot Chat's OTel spool file and exports
//! it to the governed collector over OTLP/HTTP.
//!
//! Copilot Chat can be told to write telemetry to a file
//! (`github.copilot.chat.otel.exporterType = "file"`, `outfile = <path>`) --
//! which `configure` does not set today, so this is opt-in. Nothing in VS Code
//! then drains that file. This does, on whatever schedule the developer's
//! systemd timer or launchd agent runs it (sample units in
//! `docs/governance-auth/commands.md`; installing them is deliberately **not**
//! this binary's job).
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
//! ## And the second: the checkpoint only moves after a 2xx
//!
//! The offset advances after the collector has accepted the batch, never
//! before. A rejected or unreachable collector leaves the offset where it was,
//! so the same bytes are re-read next run. The spool itself is never written
//! to at all -- see [`spool`]'s module doc for why truncating a file VS Code
//! holds open is unsafe on both target platforms.

mod batch;
mod checkpoint;
mod logs;
mod metrics;
mod otlp;
mod push;
mod record;
mod spool;
mod status;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use checkpoint::Checkpoint;
pub use status::SpoolStatus;

use crate::{config::OauthConfig, oauth};

/// Runs one drain.
///
/// `dry_run` still authenticates and still parses (so it reports exactly what
/// a real run would send), but posts nothing and moves nothing.
pub async fn run(http: &reqwest::Client, config: &OauthConfig, dry_run: bool) -> Result<()> {
    let endpoint = config.otel_endpoint.as_deref().context(
        "no collector configured: supply --otel-endpoint / GOVERNANCE_AUTH_OTEL_ENDPOINT (or set \
         `otel_endpoint` in your config file) before running `copilot-push`",
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

    let checkpoint_path = checkpoint::path(&crate::cache::state_dir()?);
    let mut state = checkpoint::load(&checkpoint_path)?;

    let drained = spool::drain(&spool_path, state.offset)?;
    if drained.restarted {
        eprintln!(
            "The spool at {} is shorter than the recorded offset ({} bytes): it was truncated or \
             rotated, so the drain restarted at byte 0.",
            spool_path.display(),
            state.offset
        );
    }

    if drained.lines.is_empty() {
        eprintln!(
            "Nothing new in {} ({} bytes, offset {}).",
            spool_path.display(),
            drained.size,
            drained.next_offset
        );
        // Still record a restart, so the next run does not re-detect it.
        return finish(
            &checkpoint_path,
            &mut state,
            drained.next_offset,
            0,
            dry_run,
        );
    }

    let batch = batch::build(&drained.lines);
    eprintln!("{}", batch.counts.describe());

    if dry_run {
        eprintln!(
            "--dry-run: nothing was posted and the checkpoint stays at byte {}.",
            state.offset
        );
        return Ok(());
    }

    if let Some(payload) = &batch.metrics {
        push::post(http, endpoint, push::Signal::Metrics, &bearer, payload).await?;
    }
    if let Some(payload) = &batch.logs {
        push::post(http, endpoint, push::Signal::Logs, &bearer, payload).await?;
    }

    finish(
        &checkpoint_path,
        &mut state,
        drained.next_offset,
        batch.counts.total_pushed(),
        dry_run,
    )
}

/// Advances and persists the checkpoint. Reached only after every POST that
/// was going to happen has returned 2xx.
fn finish(
    path: &Path,
    state: &mut Checkpoint,
    next_offset: u64,
    pushed: u64,
    dry_run: bool,
) -> Result<()> {
    if dry_run {
        return Ok(());
    }
    state.offset = next_offset;
    state.last_push_records = pushed;
    if pushed > 0 {
        state.last_push_unix = Some(checkpoint::now_unix()?);
    }
    checkpoint::store(path, state)?;
    if pushed > 0 {
        eprintln!("Pushed {pushed} record(s); checkpoint at byte {next_offset}.");
    }
    Ok(())
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
