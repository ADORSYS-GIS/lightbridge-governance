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
//! ## Why a failed pass still moves an offset
//!
//! [`export::signal`] resolves a range left to right and reports the prefix it
//! got through, so "the pass failed" and "the pass delivered nothing" are
//! different answers. Advancing over the prefix regardless is the whole fix
//! for the duplicate-delivery defect: what the collector took is recorded the
//! moment the pass ends, however it ends.
//!
//! ## When the loss counter moves
//!
//! Records lost in the transform are attributed to the bytes the **shared**
//! offset just advanced over -- the range both signals now agree on. Counting
//! them over the whole drain would count them again on the next run, which
//! re-reads whatever the shared offset did not cover. Records the collector
//! refused are counted immediately instead: that signal's offset has moved
//! past them, so nothing will re-discover them.

use std::path::Path;

use anyhow::{Context, Result};

use super::{
    batch, checkpoint,
    checkpoint::Checkpoint,
    lock,
    pass::{self, Endpoint, Outcome, Target},
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

    eprintln!("{}", batch::build(&drained.lines).counts.describe());

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
    let now = checkpoint::now_unix()?;
    let target = Target {
        lines: &drained.lines,
        end_offset: drained.next_offset,
        now,
    };
    let to = Endpoint {
        http,
        base: endpoint,
        bearer,
    };
    let outcome = pass::both(&to, &mut state, target).await;

    // The transform's loss is final only for the bytes the SHARED offset just
    // covered; anything past it is re-read, and re-counted, next run.
    let delivered: Vec<&Line> = drained
        .lines
        .iter()
        .filter(|line| line.offset < state.offset)
        .collect();
    let lost_in_transform = batch::build(&delivered).counts.discarded();
    state.record_discards(outcome.discarded.saturating_add(lost_in_transform))?;
    // Only on an actual push: these two describe the last delivery, and a run
    // that delivered nothing did not make one. Overwriting them with zeros
    // would erase the evidence that a push ever succeeded, which is exactly
    // what `status` uses to tell a stalled timer from a fresh install.
    if outcome.pushed > 0 {
        state.last_push_records = outcome.pushed;
        state.last_push_unix = Some(now);
    }
    if state != before {
        checkpoint::store(&checkpoint_path, &state)?;
    }
    report(&state, &outcome, lost_in_transform);

    pass::settled(&outcome)
        .context("the collector did not accept every signal; those bytes stay pending")
}

fn report(state: &Checkpoint, outcome: &Outcome, lost: u64) {
    if outcome.pushed > 0 {
        eprintln!(
            "Pushed {} record(s); checkpoint at byte {}.",
            outcome.pushed, state.offset
        );
    }
    if outcome.held > 0 {
        eprintln!(
            "{} record(s) the collector refused on their own are held, not discarded: a single \
             400 can come from a proxy rather than from the payload. The next wake decides.",
            outcome.held
        );
    }
    let discarded = outcome.discarded.saturating_add(lost);
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
