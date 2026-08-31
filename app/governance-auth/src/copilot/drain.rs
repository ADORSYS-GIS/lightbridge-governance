//! One authenticated pass over the spool: lock, read, transform, export,
//! record.
//!
//! Split out of [`super`] so that module stays what it says it is -- the
//! fail-closed ordering and nothing else. Everything here runs only after a
//! bearer exists, which is why none of it takes a config or can reach the
//! authorization server.
//!
//! ## Where the two signals part company
//!
//! Metrics and logs are posted to different endpoints and are accepted or
//! refused independently, so they carry independent offsets. With one shared
//! offset, "metrics accepted, logs rejected" left the whole range pending and
//! re-posted the *accepted* metrics on every later wake -- duplicated usage
//! data that nothing on this side could notice, because from here the run had
//! simply failed. Both are attempted every run for the same reason: a failure
//! on one says nothing about the other.
//!
//! ## When the loss counter moves
//!
//! Records lost in the transform are attributed to the range the **shared**
//! offset advances over -- the byte both signals agree on. Counting them when
//! either signal advanced would count them again on the next run, which
//! re-reads the same lines because the shared offset did not move. Records the
//! collector refused are counted immediately instead: they are per-signal and
//! that signal's offset has moved past them, so nothing will re-discover them.

use std::path::Path;

use anyhow::{Context, Result};

use super::{
    batch, checkpoint,
    checkpoint::Checkpoint,
    export, lock,
    push::Signal,
    spool::{self, Line},
};
use crate::redacted::Redacted;

pub async fn once(
    http: &reqwest::Client,
    endpoint: &str,
    bearer: &Redacted<String>,
    spool_path: &Path,
    dry_run: bool,
) -> Result<()> {
    let state_dir = crate::cache::state_dir()?;
    // Held for the whole read-modify-write below. See `lock`'s module doc.
    let _guard = lock::acquire(&state_dir)?;

    let checkpoint_path = checkpoint::path(&state_dir);
    let mut state = checkpoint::load(&checkpoint_path)?;

    let drained = spool::drain(spool_path, state.offset)?;
    if drained.missing {
        eprintln!(
            "There is no spool at {}. Nothing was drained and the checkpoint stays at byte {} -- \
             a path that is not there says nothing about how far the real spool got. Check \
             --copilot-spool-path if this is unexpected.",
            spool_path.display(),
            state.offset
        );
        return Ok(());
    }
    if drained.restarted {
        eprintln!(
            "The spool at {} is shorter than the recorded offset ({} bytes): it was truncated or \
             rotated, so the drain restarted at byte 0.",
            spool_path.display(),
            state.offset
        );
        state.restart();
    }

    if drained.lines.is_empty() {
        eprintln!(
            "Nothing new in {} ({} bytes, offset {}).",
            spool_path.display(),
            drained.size,
            drained.next_offset
        );
        // A rotation is the one thing an empty drain still has to persist, or
        // the next run re-detects and re-reports the same restart.
        if drained.restarted && !dry_run {
            checkpoint::store(&checkpoint_path, &state)?;
        }
        return Ok(());
    }

    let counts = batch::build(&drained.lines).counts;
    eprintln!("{}", counts.describe());
    let lost_in_transform = counts.discarded();

    if dry_run {
        eprintln!(
            "--dry-run: nothing was posted and the checkpoint stays at byte {}.",
            state.offset
        );
        return Ok(());
    }

    // The state as it was, so a run that changed nothing writes nothing. A
    // failed export must not so much as create the checkpoint file: "there is
    // no checkpoint" is what a drain that has never delivered anything looks
    // like, and `status` says so.
    let before = state.clone();
    let mut failures = Vec::new();
    let mut pushed: u64 = 0;
    let mut refused: u64 = 0;

    for signal in [Signal::Metrics, Signal::Logs] {
        let pending = pending_for(&state, signal, &drained.lines);
        match export::signal(http, endpoint, signal, bearer, &pending).await {
            Ok(done) => {
                state.advance(signal, drained.next_offset);
                pushed = pushed.saturating_add(u64::try_from(done.accepted).unwrap_or(u64::MAX));
                refused = refused.saturating_add(u64::try_from(done.refused).unwrap_or(u64::MAX));
            }
            Err(error) => failures.push(format!("{error:#}")),
        }
    }

    // Only once BOTH signals have cleared the range is the transform's loss
    // final; until then the next run re-reads these lines and re-counts them.
    let settled = state.offset >= drained.next_offset;
    state.record_discards(refused.saturating_add(if settled { lost_in_transform } else { 0 }))?;
    // Only on an actual push: these two describe the last delivery, and a run
    // that delivered nothing did not make one. Overwriting them with zeros
    // would erase the evidence that a push ever succeeded, which is exactly
    // what `status` uses to tell a stalled timer from a fresh install.
    if pushed > 0 {
        state.last_push_records = pushed;
        state.last_push_unix = Some(checkpoint::now_unix()?);
    }
    if state != before {
        checkpoint::store(&checkpoint_path, &state)?;
    }
    report(
        &state,
        pushed,
        refused,
        settled.then_some(lost_in_transform),
    );

    if failures.is_empty() {
        return Ok(());
    }
    Err(anyhow::anyhow!(failures.join("; ")))
        .context("the collector did not accept every signal; those bytes stay pending")
}

/// The lines one signal has not yet delivered. Borrowed, not cloned: a full
/// drain is megabytes of strings and this runs twice per pass.
fn pending_for<'a>(state: &Checkpoint, signal: Signal, lines: &'a [Line]) -> Vec<&'a Line> {
    let from = state.signal_offset(signal);
    lines.iter().filter(|line| line.offset >= from).collect()
}

fn report(state: &Checkpoint, pushed: u64, refused: u64, lost: Option<u64>) {
    if pushed > 0 {
        eprintln!(
            "Pushed {pushed} record(s); checkpoint at byte {}.",
            state.offset
        );
    }
    let discarded = refused.saturating_add(lost.unwrap_or_default());
    if discarded > 0 {
        eprintln!(
            "⚠️  discarded {discarded} record(s) that will never reach the collector ({} in \
             total since this checkpoint was created). `governance-auth status` shows this until \
             it ages out; a number that climbs after a VS Code update means the spool's shapes \
             moved and this parser needs revisiting.",
            state.discarded_total
        );
    }
}
