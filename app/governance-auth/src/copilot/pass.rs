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

use super::{checkpoint::Checkpoint, export, journal::Journal, push::Signal, spool::Line};
use crate::redacted::Redacted;

#[derive(Default)]
pub struct Outcome {
    pub pushed: u64,
    /// Records refused on their own for the first time. Held for another
    /// wake's evidence rather than discarded -- see [`super::quarantine`].
    pub held: u64,
    /// A signal stopped on a refused record with nothing after it. The only
    /// stall a later wake does not clear on its own -- see
    /// [`export::Progress::exhausted`].
    pub stalled: bool,
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

/// Offers both signals; each one's offset moves over the prefix it resolved.
///
/// ⚠️ The offsets are no longer advanced *here*. [`export::signal`] records the
/// prefix into `journal` as it moves, because a wake that is killed part way
/// through never reaches this function's return -- see [`super::journal`].
/// What is left here is the tally and the quarantine's per-wake pruning.
pub async fn both(to: &Endpoint<'_>, journal: &mut Journal<'_>, target: Target<'_>) -> Outcome {
    let mut outcome = Outcome::default();
    let now = target.now;
    journal.quarantine().prune(now);

    for signal in [Signal::Metrics, Signal::Logs] {
        let pending = pending_for(journal.state(), signal, target.lines);
        let mut offer = export::Offer {
            http: to.http,
            base: to.base,
            signal,
            bearer: to.bearer,
            // Reborrowed rather than moved: the second signal needs it too.
            journal: &mut *journal,
            now,
            end_offset: target.end_offset,
        };
        let progress = export::signal(&mut offer, &pending).await;

        outcome.pushed = outcome.pushed.saturating_add(count(progress.accepted));
        outcome.held = outcome.held.saturating_add(count(progress.held));
        outcome.stalled |= progress.exhausted;
        if let Some(error) = progress.stopped {
            outcome.failures.push(format!("{error:#}"));
        }
    }

    outcome
}

/// What one sweep came to, on stderr. Lives here rather than with the sweep
/// because every field of it is an [`Outcome`] this module built.
pub fn report(journal: &Journal<'_>, outcome: &Outcome) {
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
