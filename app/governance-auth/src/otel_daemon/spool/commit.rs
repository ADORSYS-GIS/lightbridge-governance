//! Durably advancing past a record: delivered, refused, or given up on.

use std::{fs::OpenOptions, io::ErrorKind};

use anyhow::{Context, Result};

use super::{DurableSpool, Pending, checkpoint};

/// Leave a spool smaller than this alone. Mirrors
/// [`crate::copilot::spool::reclaim::RECLAIM_ABOVE`] and its reasoning: not
/// worth the (narrow, documented) truncate race for a few kilobytes.
///
/// `pub(super)`, not private: `spool::tests` needs it to build a spool
/// guaranteed to cross the threshold in one record.
pub(super) const RECLAIM_ABOVE: u64 = 1024 * 1024;

/// The minimum wall-clock gap between two refusals of the same record that
/// count as genuinely *separate* evidence (#269/#291 review round 2, P2).
/// `Quarantine::refused`'s "separate wakes" reasoning assumes attempts are
/// time-decorrelated -- true of Copilot's own ~5-minute wake, but this
/// daemon's `pump` retries every 5 seconds (`drain::PUMP_INTERVAL`), and
/// `drain_retained` can retry again on every admitted request besides. Two
/// refusals landing inside the same brief flaky window (a WAF or proxy blip)
/// used to satisfy condition 1 almost immediately. 60 seconds is well above
/// one `pump` interval -- a deterministically bad payload still clears it on
/// the next tick past the gap, but a transient blip has to span a full
/// minute to fool it, which the measured half-400-gateway case this rule
/// guards against does not.
///
/// `pub(super)`: `spool::tests` needs it to space out synthetic timestamps in
/// a test that does not wait on real wall-clock time.
pub(super) const MIN_SEPARATION_SECONDS: u64 = 60;

impl DurableSpool {
    /// Durably advances past a delivered record.
    pub fn advance(&mut self, pending: &Pending) -> Result<()> {
        self.commit_past(pending.boundary, 0)
    }

    /// Records one more attempt's refusal of `pending` and answers whether it
    /// has now been refused on enough separate attempts to be *eligible* for
    /// discard -- eligible, not discarded: [`super::super::drain`]'s caller
    /// still owes the second condition (has the collector been shown to
    /// accept something else) before acting on `true`. Never itself discards
    /// -- see [`Self::discard_confirmed`].
    ///
    /// `now` is taken explicitly, not read internally -- unlike
    /// [`Self::advance`]/[`Self::discard_confirmed`], which have nothing to
    /// gate on time -- so a test can pin two attempts at a deterministic gap
    /// apart rather than depending on real wall-clock elapsing between two
    /// calls (see [`MIN_SEPARATION_SECONDS`]).
    pub fn record_refusal(&mut self, pending: &Pending, now: u64) -> Result<bool> {
        let eligible =
            self.checkpoint
                .quarantine
                .refused(&pending.key, now, MIN_SEPARATION_SECONDS);
        checkpoint::store(&self.checkpoint_path, &self.checkpoint)?;
        Ok(eligible)
    }

    /// Durably discards `stuck` -- once a caller has confirmed BOTH
    /// quarantine conditions: refused on its own across enough separate
    /// attempts ([`Self::record_refusal`]), and the collector shown to accept
    /// something else, which for the daemon's stream is `probe`, the next
    /// record after `stuck` that was just offered on its own and accepted
    /// (#269/#291 review, P1-3; mirrors `copilot::export::isolate`'s same two
    /// conditions for a batched drain). Commits through `probe`'s boundary in
    /// one write, so its own delivery is recorded in the same commit that
    /// discards `stuck` -- no separate `advance` call needed, and no window
    /// where the probe is "delivered but not yet durable" while `stuck` is
    /// still pending.
    pub fn discard_confirmed(&mut self, stuck: &Pending, probe: &Pending) -> Result<()> {
        self.checkpoint.quarantine.forget(&stuck.key);
        self.commit_past(probe.boundary, 1)
    }

    /// Durably advances the offset to `boundary`, charging `lost` records to
    /// `discarded_total`, persists, and reclaims the file if that leaves it
    /// fully delivered. `pub(super)`: [`super::read`] also needs it directly,
    /// to skip an undecodable line (a torn write, not a refusal -- there is
    /// no record here for [`Self::discard_confirmed`]'s two conditions to
    /// apply to).
    pub(super) fn commit_past(&mut self, boundary: u64, lost: u64) -> Result<()> {
        self.checkpoint.spool.clone_from(&self.pending_identity);
        self.checkpoint.offset = boundary;
        self.checkpoint.record_discard(lost)?;
        checkpoint::store(&self.checkpoint_path, &self.checkpoint)?;
        self.try_reclaim()
    }

    /// Truncates the spool once every byte in it has been delivered or given
    /// up on. Mirrors [`crate::copilot::spool::reclaim`]'s precondition
    /// (`size == offset`, re-read from the open descriptor immediately before
    /// truncating) and its residual race -- see that module's doc for the
    /// measured window. This daemon is the spool's *only* writer, which
    /// narrows the race further than Copilot's case (no external process
    /// holding descriptors), but does not close it: a `retain` landing
    /// between the `fstat` and the `set_len` is still destroyed undelivered.
    /// Nothing here is fatal; a spool that could not be reclaimed just stays
    /// larger than [`RECLAIM_ABOVE`] until the next caught-up pass.
    fn try_reclaim(&mut self) -> Result<()> {
        if self.checkpoint.offset <= RECLAIM_ABOVE {
            return Ok(());
        }
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.spool_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("opening {} to reclaim it", self.spool_path.display())
                });
            }
        };
        let size = file
            .metadata()
            .with_context(|| format!("sizing {}", self.spool_path.display()))?
            .len();
        if size != self.checkpoint.offset {
            return Ok(());
        }
        file.set_len(0)
            .with_context(|| format!("truncating {}", self.spool_path.display()))?;
        self.checkpoint.restart();
        // `None`, not a freshly-computed identity of the (now empty) file:
        // `identity::restart`'s "unknown is not a mismatch" rule already
        // treats `None` as adoptable against anything, and offset 0 means the
        // size check can never read as a truncation either -- so the very
        // next tail simply adopts whatever is there next, with no special
        // case needed for "the file we just emptied ourselves".
        self.checkpoint.spool = None;
        checkpoint::store(&self.checkpoint_path, &self.checkpoint)
    }
}
