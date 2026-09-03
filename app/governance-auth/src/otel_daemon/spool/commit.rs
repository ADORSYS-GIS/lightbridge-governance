//! Durably advancing past a record: delivered, or given up on.

use std::{
    fs::OpenOptions,
    io::ErrorKind,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};

use super::{DurableSpool, Pending, checkpoint};

/// Leave a spool smaller than this alone. Mirrors
/// [`crate::copilot::spool::reclaim::RECLAIM_ABOVE`] and its reasoning: not
/// worth the (narrow, documented) truncate race for a few kilobytes.
///
/// `pub(super)`, not private: `spool::tests` needs it to build a spool
/// guaranteed to cross the threshold in one record.
pub(super) const RECLAIM_ABOVE: u64 = 1024 * 1024;

impl DurableSpool {
    /// Durably advances past a delivered record.
    pub fn advance(&mut self, pending: &Pending) -> Result<()> {
        self.commit_past(pending.boundary, 0)
    }

    /// Records one more separate refusal of `pending`. Once it has been
    /// refused on its own across
    /// [`crate::copilot::quarantine::REFUSALS_BEFORE_DISCARD`] attempts, it is
    /// given up on and skipped (`Ok(true)`); otherwise the checkpoint's
    /// quarantine table is updated but the offset is not moved, so the same
    /// record is retried next time (`Ok(false)`).
    pub fn quarantine_or_discard(&mut self, pending: &Pending) -> Result<bool> {
        let now = now_unix()?;
        let discard = self.checkpoint.quarantine.refused(&pending.key, now);
        if discard {
            self.checkpoint.quarantine.forget(&pending.key);
            self.commit_past(pending.boundary, 1)?;
        } else {
            checkpoint::store(&self.checkpoint_path, &self.checkpoint)?;
        }
        Ok(discard)
    }

    /// Advances the offset to `boundary`, charges `lost` records to
    /// `discarded_total`, persists, and reclaims the file if that leaves it
    /// fully delivered.
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

pub(super) fn now_unix() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("reading the system clock")?
        .as_secs())
}
