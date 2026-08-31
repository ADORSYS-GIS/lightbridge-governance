//! Offering one wake's lines to both signals, and recording how far each one
//! got.
//!
//! Split out of [`super::drain`] to keep both under the 200-LoC gate. What
//! lives here is the *bookkeeping* half: which lines a signal still owes, how
//! far its offset may move afterwards, and what the wake as a whole came to.
//! [`super::export`] decides what to post; [`super::drain`] decides what to
//! persist.
//!
//! ## The one rule worth reading twice
//!
//! Each signal's offset moves over the prefix its pass **resolved**, whether
//! the pass finished or stopped. It used to move only on `Ok`, so a pass that
//! ran out of its request budget -- or lost the connection mid-split -- threw
//! away every acceptance it had already obtained, and the next wake rebuilt
//! and re-sent all of them. Measured at 512 records with 12 refused: 438 good
//! records delivered again on every wake, for ever, into a usage store.

use anyhow::Result;

use super::{checkpoint::Checkpoint, export, push::Signal, spool::Line};
use crate::redacted::Redacted;

#[derive(Default)]
pub struct Outcome {
    pub pushed: u64,
    pub discarded: u64,
    /// Records refused on their own for the first time. Held for another
    /// wake's evidence rather than discarded -- see [`super::quarantine`].
    pub held: u64,
    pub failures: Vec<String>,
}

/// What one wake has to offer: the lines it read, the byte after the last of
/// them, and the clock reading every decision in the pass is made against.
pub struct Target<'a> {
    pub lines: &'a [Line],
    pub end_offset: u64,
    pub now: u64,
}

/// Where to post and with what. Grouped so [`both`] stays under clippy's
/// argument ceiling and so the two halves of its input read apart: this is the
/// destination, [`Target`] is the data.
pub struct Endpoint<'a> {
    pub http: &'a reqwest::Client,
    pub base: &'a str,
    pub bearer: &'a Redacted<String>,
}

/// Offers both signals and moves each one's offset over the prefix it
/// resolved. Takes `state` by `&mut` because the quarantine it carries is
/// evidence that has to survive into the checkpoint whether the pass succeeded
/// or not.
pub async fn both(to: &Endpoint<'_>, state: &mut Checkpoint, target: Target<'_>) -> Outcome {
    let mut outcome = Outcome::default();
    let now = target.now;
    // Moved out so the borrow checker sees one mutable path into `state`; put
    // back below, pruned, whatever happened.
    let mut quarantine = std::mem::take(&mut state.quarantine);
    quarantine.prune(now);

    for signal in [Signal::Metrics, Signal::Logs] {
        let pending = pending_for(state, signal, target.lines);
        let mut offer = export::Offer {
            http: to.http,
            base: to.base,
            signal,
            bearer: to.bearer,
            quarantine: &mut quarantine,
            now,
        };
        let progress = export::signal(&mut offer, &pending).await;

        // ⚠️ Independent of whether the pass succeeded, and that is the fix: a
        // pass that stopped short still delivered its prefix, and not
        // recording that is what made the next wake re-send it. The `>` guard
        // is not an optimisation -- a pass that resolved nothing must leave
        // the checkpoint untouched, including not creating the file, because
        // "no checkpoint" is how a drain that has never delivered anything is
        // meant to look.
        let reached = export::advanced_to(&pending, progress.resolved, target.end_offset);
        if reached > state.signal_offset(signal) {
            state.advance(signal, reached);
        }
        outcome.pushed = outcome.pushed.saturating_add(count(progress.accepted));
        outcome.discarded = outcome.discarded.saturating_add(count(progress.discarded));
        outcome.held = outcome.held.saturating_add(count(progress.held));
        if let Some(error) = progress.stopped {
            outcome.failures.push(format!("{error:#}"));
        }
    }

    state.quarantine = quarantine;
    outcome
}

/// The wake's own failure, if it had one. `Ok` means every line read was
/// resolved.
pub fn settled(outcome: &Outcome) -> Result<()> {
    if outcome.failures.is_empty() {
        return Ok(());
    }
    Err(anyhow::anyhow!(outcome.failures.join("; ")))
}

fn count(records: usize) -> u64 {
    u64::try_from(records).unwrap_or(u64::MAX)
}

/// The lines one signal has not yet delivered. Borrowed, not cloned: a full
/// drain is megabytes of strings and this runs twice per pass.
fn pending_for<'a>(state: &Checkpoint, signal: Signal, lines: &'a [Line]) -> Vec<&'a Line> {
    let from = state.signal_offset(signal);
    lines.iter().filter(|line| line.offset >= from).collect()
}
