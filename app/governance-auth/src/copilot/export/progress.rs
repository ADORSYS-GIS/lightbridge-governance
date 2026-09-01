//! What one pass over one signal is carrying: the offer it posts from, the
//! tally it builds up, and the point at which that tally becomes durable.
//!
//! Split out of [`super`] so that module stays the traversal and nothing else,
//! and to keep both under the 200-LoC gate.

use anyhow::{Error, Result};
use serde_json::Value;

use crate::{
    copilot::{batch, journal::Journal, push::Signal, spool::Line},
    redacted::Redacted,
};

/// Everything one pass over one signal did. `resolved` is the only field the
/// offset depends on, and it is a prefix length precisely so that a pass which
/// stopped early is still worth something.
#[derive(Debug, Default)]
pub struct Progress {
    /// Lines of the offered slice that are resolved, counted from the front:
    /// delivered, given up on, or carrying nothing for this signal.
    pub resolved: usize,
    /// Records the collector accepted.
    pub accepted: usize,
    /// Records given up on. These are gone: the caller records them as
    /// discarded and `status` shows the loss.
    pub discarded: usize,
    /// Records refused for the first time and held for another wake's
    /// evidence. Reported so "waiting on a second opinion" does not read as a
    /// stall.
    pub held: usize,
    /// The pass stopped on a refused record with **nothing after it** to prove
    /// the collector with. Unlike every other stop, no later wake resolves
    /// this one: it clears when a new record is appended to the spool and not
    /// before. Reported so `status` can say so instead of advising a re-run
    /// that will do exactly the same thing.
    pub exhausted: bool,
    pub requests: usize,
    /// Why the pass stopped short of the whole range. `None` means it did not.
    pub stopped: Option<Error>,
    /// The prefix length last made durable. `None` until the first commit, so
    /// that a pass with nothing to offer -- one whose signal is already ahead
    /// of every line read -- still settles its offset exactly as it did before
    /// progress became incremental.
    committed: Option<usize>,
    /// `discarded` as of that commit, so a commit charges only what the
    /// previous one did not.
    committed_discards: usize,
}

/// The per-pass state [`super::signal`] needs beyond the lines themselves. A
/// struct rather than eight parameters, and the journal is `&mut` because both
/// the refusal counts it carries and the progress it records have to survive
/// into the checkpoint.
pub struct Offer<'a, 'j> {
    pub http: &'a reqwest::Client,
    pub base: &'a str,
    pub signal: Signal,
    pub bearer: &'a Redacted<String>,
    pub journal: &'a mut Journal<'j>,
    pub now: u64,
    /// The byte after the last line this wake read: where a fully-resolved
    /// range advances this signal's offset to.
    pub end_offset: u64,
}

/// `None` when this range carries nothing for `signal`.
pub fn build(lines: &[&Line], signal: Signal) -> Option<(Value, usize)> {
    let (payload, records) = batch::build(lines).signal(signal);
    Some((payload?, records))
}

/// The byte the caller may advance this signal's offset to: the start of the
/// first *unresolved* line, or `end_of_range` when the whole slice resolved.
fn advanced_to(lines: &[&Line], resolved: usize, end_of_range: u64) -> u64 {
    lines.get(resolved).map_or(end_of_range, |line| line.offset)
}

/// Makes everything resolved since the last call durable.
///
/// Called after every prefix advance rather than once at the end of the pass,
/// which is the whole point -- see [`crate::copilot::journal`]. It is a no-op
/// when the prefix has not moved, so calling it defensively costs nothing.
pub fn commit(offer: &mut Offer<'_, '_>, progress: &mut Progress, lines: &[&Line]) -> Result<()> {
    if progress.committed == Some(progress.resolved) {
        return Ok(());
    }
    let reached = advanced_to(lines, progress.resolved, offer.end_offset);
    let refused = progress
        .discarded
        .saturating_sub(progress.committed_discards);
    offer.journal.advance(
        offer.signal,
        reached,
        u64::try_from(refused).unwrap_or(u64::MAX),
    )?;
    progress.committed = Some(progress.resolved);
    progress.committed_discards = progress.discarded;
    Ok(())
}
