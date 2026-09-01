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
//!
//! Both of those now happen inside [`super::journal`], in the same write as
//! the offset they belong to. An offset that survives a kill and a loss count
//! that does not would resume past records nothing ever counted, which is the
//! conservation rule breaking at every interrupted wake rather than never.

use std::path::Path;

use anyhow::{Context, Result};

use super::{
    batch, checkpoint,
    journal::Journal,
    lock,
    pass::{self, Endpoint, Outcome, Target},
    spool::{self, Restart},
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
    let state = checkpoint::load(&checkpoint_path)?;

    let drained = spool::drain(spool_path, state.offset, state.spool.as_ref())?;
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

    if let Some(restart) = drained.restarted {
        eprintln!("{}", restarted(spool_path, restart, state.offset));
    }
    let mut journal = Journal::new(
        checkpoint_path,
        &drained.lines,
        state,
        drained.identity,
        drained.restarted.is_some(),
    );

    if drained.lines.is_empty() {
        eprintln!(
            "Nothing new in {} ({} bytes, offset {}).",
            spool_path.display(),
            drained.size,
            drained.next_offset
        );
        // A rotation is the one thing an empty drain still has to persist, or
        // the next run re-detects and re-reports the same restart.
        if drained.restarted.is_some() && !dry_run {
            journal.commit()?;
        }
        return Ok(());
    }

    eprintln!("{}", batch::build(&drained.lines).counts.describe());

    if dry_run {
        eprintln!(
            "--dry-run: nothing was posted and the checkpoint stays at byte {}.",
            journal.state().offset
        );
        return Ok(());
    }

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
    let outcome = pass::both(&to, &mut journal, target).await;

    journal.finished(outcome.pushed, outcome.stalled, now);
    journal.commit()?;
    report(&journal, &outcome);

    pass::settled(&outcome)
        .context("the collector did not accept every signal; those bytes stay pending")
}

/// The two restarts read differently on purpose: one sends the reader looking
/// for a truncation and the other for a file swap, and being sent to the wrong
/// one is worse than being told nothing.
fn restarted(path: &Path, why: Restart, offset: u64) -> String {
    match why {
        Restart::Truncated => format!(
            "The spool at {} is shorter than the recorded offset ({offset} bytes): it was \
             truncated or rotated, so the drain restarted at byte 0.",
            path.display()
        ),
        Restart::Replaced => format!(
            "The spool at {} is not the file byte {offset} was measured against -- it was \
             replaced, not appended to (VS Code recreating its outfile does this). The drain \
             restarted at byte 0, so records the old file already delivered may arrive again if \
             the two files share content.",
            path.display()
        ),
    }
}

fn report(journal: &Journal<'_>, outcome: &Outcome) {
    let state = journal.state();
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
    if outcome.stalled {
        eprintln!(
            "The refused record is the LAST one in the spool, so there is nothing after it to \
             prove the collector still works with -- and giving up on no evidence is how a \
             misconfigured collector empties a spool. This does not clear on the next wake: it \
             clears when Copilot writes another record. Re-running now repeats exactly this."
        );
    }
    let discarded = journal.lost();
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
