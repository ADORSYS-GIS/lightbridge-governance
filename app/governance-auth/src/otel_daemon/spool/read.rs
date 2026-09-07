//! Tailing the durable spool for the next undelivered record.

use anyhow::{Context, Result};

use super::{DurableSpool, Pending, envelope};
use crate::copilot::{quarantine::Quarantine, spool as tail, spool::Identity};

/// What one read starting at a given offset found, before anything is done
/// about it -- kept apart from acting on it so [`DurableSpool::peek_next`]
/// can share the exact same read+decode path as [`DurableSpool::next`]
/// without being able to mutate anything on the strength of a look.
enum Peeked {
    /// Nothing pending at or after this offset yet.
    Empty,
    /// The spool was truncated or replaced under us; the caller decides
    /// whether adopting that (restarting the tail at byte 0) is theirs to do.
    Restarted,
    /// The next line failed to decode; `boundary` is where it ends, for a
    /// caller that is allowed to skip past it.
    Undecodable {
        boundary: u64,
    },
    Decoded(Pending),
}

impl DurableSpool {
    /// The next undelivered record, decoding and skipping past anything that
    /// fails to parse (see [`super`]'s module doc's torn-write paragraph).
    /// `Ok(None)` once the spool is caught up.
    pub fn next(&mut self) -> Result<Option<Pending>> {
        let now = super::checkpoint::now_unix()?;
        self.checkpoint.quarantine.prune(now);
        loop {
            let (peeked, identity) = self.peek_at(self.checkpoint.offset)?;
            // Adopted into the checkpoint only when something durable
            // actually happens -- mirrors `copilot::journal::Journal`'s same
            // deferral, for the same reason: observing it must never itself
            // be a reason to write. `commit_past` is what persists it.
            self.pending_identity = identity;
            match peeked {
                Peeked::Empty => return Ok(None),
                Peeked::Restarted => {
                    tracing::warn!(
                        "the daemon's durable spool was rotated unexpectedly; restarting the \
                         tail at byte 0"
                    );
                    self.checkpoint.restart();
                }
                Peeked::Undecodable { boundary } => {
                    tracing::warn!("a durable spool record could not be decoded; discarding it");
                    self.commit_past(boundary, 1)?;
                }
                Peeked::Decoded(pending) => return Ok(Some(pending)),
            }
        }
    }

    /// Whether nothing is pending at the checkpoint offset -- routed through
    /// the same identity-aware read [`Self::next`] uses, not a raw
    /// `size <= offset` compare (#269/#291 review round 2, P1). That matters
    /// because a crash between [`super::commit`]'s `try_reclaim` truncating
    /// the file and its checkpoint reset landing leaves disk state
    /// `{checkpoint: offset=N (stale, large), file: truncated to 0}` -- a
    /// plain size compare reads `size <= N` as "caught up" forever, even once
    /// new records are appended starting from byte 0, wedging the drain
    /// permanently (the agent keeps getting `202`, nothing is ever offered to
    /// the collector). [`Self::peek_at`] already detects exactly this as
    /// [`Peeked::Restarted`], which this reads as "not empty" -- `next` then
    /// adopts the restart on the very next call, self-healing.
    pub(super) fn is_caught_up(&self) -> Result<bool> {
        Ok(matches!(
            self.peek_at(self.checkpoint.offset)?.0,
            Peeked::Empty
        ))
    }

    /// The record right after `after` -- **not** the checkpoint offset -- used
    /// purely to answer "has the collector been shown to accept something
    /// else" before [`super::super::drain::advance::advance_one`] discards a record
    /// quarantine alone would give up on (#269/#291 review, P1-3). Read-only:
    /// unlike [`Self::next`], never restarts a rotated tail or skips an
    /// undecodable line on the caller's behalf -- a probe that mutated state
    /// on the mere act of looking would make peeking ahead itself a source of
    /// truth it should not be. `Ok(None)` in every case that is not a cleanly
    /// decoded record, which the caller reads as "nothing to prove the
    /// collector with right now", not as an error.
    pub fn peek_next(&self, after: &Pending) -> Result<Option<Pending>> {
        match self.peek_at(after.boundary)?.0 {
            Peeked::Decoded(pending) => Ok(Some(pending)),
            Peeked::Empty | Peeked::Restarted | Peeked::Undecodable { .. } => Ok(None),
        }
    }

    /// One read at `offset`, decoded but not acted on, alongside the
    /// identity it was read against (`None` only when there is no file yet).
    /// Shared by [`Self::next`] and [`Self::peek_next`] so the two can never
    /// drift on what counts as a record -- see [`Peeked`]'s own doc. `&self`:
    /// this never mutates, which is exactly why a probe is safe to call it.
    fn peek_at(&self, offset: u64) -> Result<(Peeked, Option<Identity>)> {
        let drained = tail::drain(&self.spool_path, offset, self.checkpoint.spool.as_ref())
            .context("tailing the daemon's durable spool")?;
        let identity = drained.identity;
        if drained.restarted.is_some() {
            return Ok((Peeked::Restarted, identity));
        }
        let Some(first) = drained.lines.first() else {
            return Ok((Peeked::Empty, identity));
        };
        // Not `drained.next_offset`, which spans every complete line that
        // read happened to return: this hands back one record at a time, so
        // it needs exactly where *that* record ends. A second line's own
        // recorded start is that boundary, computed from raw byte lengths
        // rather than the (possibly trimmed) text -- see
        // `copilot::spool::drain`. Only when there is no second line yet does
        // the read's own `next_offset` (computed the same way) apply.
        let boundary = drained
            .lines
            .get(1)
            .map_or(drained.next_offset, |second| second.offset);

        let peeked = match envelope::decode(&first.text) {
            Ok((signal, payload)) => Peeked::Decoded(Pending {
                signal,
                payload,
                key: Quarantine::key(&first.text),
                boundary,
            }),
            Err(_) => Peeked::Undecodable { boundary },
        };
        Ok((peeked, identity))
    }
}
