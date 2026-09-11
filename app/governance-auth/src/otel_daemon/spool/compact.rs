//! Compacting the spool when a dead prefix accumulates without the exact
//! "fully caught up" moment [`super::commit::try_reclaim`] requires.
//!
//! ## The gap this closes
//!
//! `try_reclaim` only truncates when `size == offset` **exactly** -- the same
//! "fully caught up" precondition `crate::copilot::spool::reclaim` already
//! documented as unreachable on a sufficiently backlogged machine (#230/#241).
//! [`super::CAPACITY`] bounds *unconsumed* bytes (`size - offset`), which
//! stays small as long as the collector keeps accepting -- but nothing bounds
//! the already-*delivered* prefix before `offset` if this daemon never once
//! has an idle instant to present that exact equality. A daemon that is
//! working perfectly (collector healthy, `offset` always advancing) can still
//! grow this file without limit under continuous enough traffic, which
//! `CAPACITY` alone was never built to catch.
//!
//! ## Why the rewrite `copilot::spool::reclaim` rejected is safe here
//!
//! That module's own doc rejects exactly this approach ("Rewrite the file to
//! keep the undelivered tail") because Copilot itself holds long-lived
//! `O_APPEND` descriptors on that file -- rewriting it races an external
//! process's own in-flight write, and a landing-mid-rewrite append would
//! interleave rather than merely be lost. **This spool has no such writer.**
//! `envelope::append_line` is the only thing that ever writes here, and it
//! opens the path fresh on every call rather than holding a persistent
//! handle, so a rename swapping in a new file underneath it is exactly as
//! transparent as it already is to a rotated log's next writer.
//!
//! ## Crash safety
//!
//! Written tmp-then-rename via [`crate::durable_state::write_durably`] (the
//! same primitive the checkpoint itself uses), then the checkpoint is reset
//! in a second, separate durable write -- mirrors `try_reclaim`'s own
//! "truncate first, checkpoint second" ordering, for the same reason: a crash
//! between the two leaves `{checkpoint: offset=N (stale), file: just the
//! compacted tail}`, which is *shorter* than the stale offset and a different
//! inode besides. That is already `crate::copilot::spool::Restart::Truncated`
//! (or `Replaced`) -- both restart the tail at byte 0 of whatever is
//! currently there, which for a file that already IS exactly the pending tail
//! is the correct answer, not a special case needing new handling.
//! [`super::read`]'s own doc names this exact crash window for
//! `try_reclaim` and calls it self-healing; nothing new is required here.

use std::fs;

use anyhow::{Context, Result};

use super::{DurableSpool, checkpoint, commit::RECLAIM_ABOVE};

impl DurableSpool {
    /// Rewrites the spool to keep only the undelivered tail once the
    /// already-delivered prefix crosses [`RECLAIM_ABOVE`], regardless of
    /// whether the spool is exactly caught up. Answers whether it compacted,
    /// purely for a test to observe.
    ///
    /// Meant to be called periodically and independently of the drain's own
    /// hot path (see `otel_daemon::spool_compaction`'s doc for why it cannot
    /// piggyback on `drain::pump`), not on every commit: a full rewrite is
    /// far more expensive than `try_reclaim`'s truncate, so this is the
    /// fallback for when that cheaper path has not been reachable, not a
    /// replacement for it.
    pub fn compact_if_stuck(&mut self) -> Result<bool> {
        if self.checkpoint.offset <= RECLAIM_ABOVE {
            return Ok(false);
        }
        let bytes = match fs::read(&self.spool_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("reading {} to compact it", self.spool_path.display())
                });
            }
        };
        let offset = usize::try_from(self.checkpoint.offset).unwrap_or(usize::MAX);
        // The file is already shorter than the offset -- a previous
        // compaction (or `try_reclaim`) already landed, or this call was
        // interrupted after a rename but before the checkpoint reset below.
        // Nothing to do here either way: the next `next()`/`is_caught_up()`
        // call already self-heals a stale offset -- see this module's own
        // doc's "Crash safety" section.
        let Some(tail) = bytes.get(offset..) else {
            return Ok(false);
        };

        let tmp = self.spool_path.with_extension("jsonl.compact.tmp");
        crate::durable_state::write_durably(&tmp, tail)?;
        let dir = self
            .spool_path
            .parent()
            .context("spool path has no parent directory")?;
        fs::rename(&tmp, &self.spool_path).with_context(|| {
            format!(
                "renaming {} to {}",
                tmp.display(),
                self.spool_path.display()
            )
        })?;
        crate::durable_state::sync_dir(dir)
            .with_context(|| format!("syncing {} after compacting the spool", dir.display()))?;

        // Mirrors `commit::try_reclaim`'s own choice: `None`, not a freshly
        // computed identity of the file just written -- "unknown is not a
        // mismatch" already makes it adoptable against whatever the next
        // tail read finds.
        self.checkpoint.offset = 0;
        self.checkpoint.spool = None;
        checkpoint::store(&self.checkpoint_path, &self.checkpoint)?;
        Ok(true)
    }
}
