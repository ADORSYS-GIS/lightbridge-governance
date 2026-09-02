//! One read -> export -> checkpoint pass over the spool.
//!
//! Called a *sweep* only because [`super::pass`] already means the narrower
//! thing: offering one sweep's lines to both signals. A wake makes as many
//! sweeps as its bounds allow -- [`super::drain`] owns the loop and the numbers.
//!
//! ## Where the two signals part company
//!
//! Metrics and logs are posted to different endpoints and are accepted or
//! refused independently, so they carry independent offsets. With one shared
//! offset, "metrics accepted, logs rejected" left the whole range pending and
//! re-posted the *accepted* metrics on every later wake -- duplicated usage
//! data that nothing on this side could notice, because from here the run had
//! simply failed. Both are attempted every sweep for the same reason: a failure
//! on one says nothing about the other.
//!
//! ## Why a failed sweep still moves an offset
//!
//! [`export::signal`] resolves a range left to right and reports the prefix it
//! got through, so "the sweep failed" and "the sweep delivered nothing" are
//! different answers. Advancing over the prefix regardless is the whole fix
//! for the duplicate-delivery defect: what the collector took is recorded the
//! moment the sweep ends, however it ends.
//!
//! ⚠️ It is also what [`Swept::complete`] is derived from, and that flag is the
//! only thing standing between the wake's loop and a conservation defect: a
//! sweep that did not resolve its whole read must be the wake's **last**. The
//! records it left behind are records it has already offered, and offering them
//! again inside the same wake would charge [`super::quarantine`] two refusals
//! for one wake's evidence -- which is exactly the rule that keeps a flaky
//! gateway from emptying the spool.
//!
//! ## When the loss counter moves
//!
//! Records lost in the transform are attributed to the bytes the **shared**
//! offset just advanced over -- the range both signals now agree on. Counting
//! them over the whole drain would count them again on the next sweep, which
//! re-reads whatever the shared offset did not cover. Records the collector
//! refused are counted immediately instead: that signal's offset has moved past
//! them, so nothing will re-discover them. Both happen inside
//! [`super::journal`], in the same write as the offset they belong to: an
//! offset that survives a kill beside a loss count that does not would resume
//! past records nothing ever counted.
//!
//! ## Why the reclaim is here rather than once per wake
//!
//! [`spool::reclaim`] may only destroy bytes the checkpoint has already passed,
//! so it runs after the checkpoint is final, never before. Both exits offer it,
//! and the *empty* one matters most: a wake that found nothing new has a spool
//! quiescent since the last wake -- what the precondition wants. Per sweep
//! rather than per wake costs an `open` and an `fstat`, and buys the case that
//! was unreachable before the loop: on a backlogged machine, the sweep that
//! finishes the backlog is the first one ever to hold `size == offset`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Error, Result};

use super::{
    batch, checkpoint,
    journal::Journal,
    pass::{self, Endpoint, Target},
    spool::{self, reclaim},
};
use crate::redacted::Redacted;

/// Everything a wake carries into every sweep it makes.
pub struct Wake<'a> {
    pub http: &'a reqwest::Client,
    pub endpoint: &'a str,
    pub bearer: &'a Redacted<String>,
    pub spool: &'a Path,
    pub checkpoint: PathBuf,
    pub dry_run: bool,
}

/// What one sweep came to.
pub struct Swept {
    /// Bytes the **shared** offset advanced over: delivered or counted, by
    /// both signals.
    pub advanced: u64,
    /// Records the collector accepted in this sweep.
    pub pushed: u64,
    /// Bytes written and not yet delivered, against the size this sweep read.
    /// A spool that grew *during* the sweep is left to the next one rather
    /// than counted here, so this never over-reports the backlog.
    pub pending: u64,
    /// Every byte this sweep read is now delivered or counted. ⚠️ The wake may
    /// sweep again only when this is true -- see the module doc.
    pub complete: bool,
    /// The collector did not take everything it was offered. The wake ends on
    /// it and exits non-zero.
    pub failed: Option<Error>,
}

impl Swept {
    /// A sweep that found nothing it could act on -- no spool, no complete
    /// line, or `--dry-run`. Never `complete`, because there is nothing for a
    /// further sweep to find either.
    fn nothing() -> Self {
        Self {
            advanced: 0,
            pushed: 0,
            pending: 0,
            complete: false,
            failed: None,
        }
    }
}

/// `carried` is what earlier sweeps in this wake already delivered. It is
/// added to this sweep's count before the checkpoint records it, so
/// `last_push_records` -- which `status` renders as "the last push" --
/// describes the whole wake. Without it a 20-sweep drain of a backlog would
/// report whatever the final, smallest sweep happened to carry.
pub async fn once(wake: &Wake<'_>, carried: u64) -> Result<Swept> {
    let state = checkpoint::load(&wake.checkpoint)?;
    let from = state.offset;

    let drained = spool::drain(wake.spool, from, state.spool.as_ref())?;
    if drained.missing {
        eprintln!(
            "There is no spool at {}. Nothing was drained and the checkpoint stays at byte {from} \
             -- a path that is not there says nothing about how far the real spool got. Check \
             --copilot-spool-path if this is unexpected.",
            wake.spool.display(),
        );
        return Ok(Swept::nothing());
    }

    if let Some(restart) = drained.restarted {
        eprintln!("{}", restart.explain(wake.spool, from));
    }
    let mut journal = Journal::new(
        wake.checkpoint.clone(),
        &drained.lines,
        state,
        drained.identity,
        drained.restarted.is_some(),
    );

    if drained.lines.is_empty() {
        eprintln!(
            "Nothing new in {} ({} bytes, offset {}).",
            wake.spool.display(),
            drained.size,
            drained.next_offset
        );
        // A rotation is the one thing an empty sweep still has to persist, or
        // the next wake re-detects and re-reports the same restart.
        if drained.restarted.is_some() && !wake.dry_run {
            journal.commit()?;
        }
        reclaim::best_effort(wake.spool, &wake.checkpoint, journal.state(), wake.dry_run);
        return Ok(Swept::nothing());
    }

    eprintln!("{}", batch::build(&drained.lines).counts.describe());

    if wake.dry_run {
        eprintln!(
            "--dry-run: nothing was posted and the checkpoint stays at byte {}.",
            journal.state().offset
        );
        return Ok(Swept::nothing());
    }

    let now = checkpoint::now_unix()?;
    let target = Target {
        lines: &drained.lines,
        end_offset: drained.next_offset,
        now,
    };
    let to = Endpoint {
        http: wake.http,
        base: wake.endpoint,
        bearer: wake.bearer,
    };
    let outcome = pass::both(&to, &mut journal, target).await;

    journal.finished(carried.saturating_add(outcome.pushed), outcome.stalled, now);
    journal.commit()?;
    pass::report(&journal, &outcome);

    // Read before the reclaim, which resets the stored offset to 0 once it
    // fires: what this sweep reached is the number the loop reasons about.
    let reached = journal.state().offset;
    reclaim::best_effort(wake.spool, &wake.checkpoint, journal.state(), wake.dry_run);

    Ok(Swept {
        advanced: reached.saturating_sub(from),
        pushed: outcome.pushed,
        pending: drained.size.saturating_sub(reached),
        complete: reached >= drained.next_offset,
        failed: pass::settled(&outcome)
            .context("the collector did not accept every signal; those bytes stay pending")
            .err(),
    })
}
