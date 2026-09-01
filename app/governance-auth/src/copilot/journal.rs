//! Making a wake's progress durable **while** it happens, not once it ends.
//!
//! ## The defect this exists to remove
//!
//! The checkpoint was written exactly once, at the end of
//! [`super::drain::once`], and this binary installs no signal handler, so a wake
//! killed part way through threw away every acceptance it had obtained and the
//! next wake re-sent all of them -- into a usage store, where a duplicate is a
//! wrong number. The sample systemd unit's own `TimeoutStartSec=` makes that
//! kill a *scheduled* one. `tests/copilot_push_interrupted.rs` has the numbers.
//!
//! ## Why not a signal handler
//!
//! A handler covers SIGTERM and covers neither SIGKILL, nor the OOM killer, nor
//! a lid closing on a flat battery. Durability that depends on catching a signal
//! is durability a `kill -9` removes. The DFS in [`super::export`] already
//! resolves a contiguous **prefix**, so "everything up to here is done" is well
//! defined at every instant of the pass; this module persists it as it moves. A
//! handler could be added on top, but as an optimisation, not the mechanism.
//!
//! ## Granularity, and why it is per prefix advance
//!
//! Every advance is immediately preceded by an HTTP round trip, which is orders
//! of magnitude slower than this write. Any coarser rule -- every N records,
//! once per batch -- re-introduces the defect in proportion to N, because
//! whatever is un-persisted when the kill lands is what gets re-delivered. So:
//! whenever the resolved prefix moves, record it. A wake that resolves nothing
//! writes nothing; one that resolves its whole range in a single accepted batch
//! writes once per signal -- the two writes it always did.
//! [`Journal::writes`] reports it; `copilot::tests::journal` pins it.
//!
//! ⚠️ There is deliberately **no `fsync`**, and that is a policy decision worth
//! disagreeing with. The threat here is *process* death -- SIGTERM, SIGKILL,
//! OOM -- against which a completed `write` + `rename` is already durable,
//! because the page cache outlives the process. `fsync` would also cover host
//! power loss, at the cost of a real disk flush per HTTP request in a large
//! drain. So a laptop losing power mid-drain still costs the duplicates this
//! module otherwise prevents: a fair trade for a five-minute developer timer,
//! and not one for anything with a stricter duty.
//!
//! ## Why the loss counter has to move with the offset
//!
//! Conservation is "no record's offset advances past unless it was delivered or
//! counted". A durable offset beside a loss count that is not breaks it at every
//! kill: the next wake resumes past records nothing counted. So a commit
//! charges, in the same write, the transform's losses inside the byte range the
//! **shared** offset has newly covered.

use std::path::PathBuf;

use anyhow::Result;

use super::{
    batch,
    checkpoint::{self, Checkpoint},
    push::Signal,
    quarantine::Quarantine,
    spool::{Identity, Line},
};

pub struct Journal<'a> {
    path: PathBuf,
    /// Every line this wake read, so a commit can charge the transform's loss to
    /// the range it is making durable rather than to end-of-wake, which a kill
    /// skips.
    lines: &'a [Line],
    state: Checkpoint,
    /// The checkpoint as it stands on disk. A wake that changed nothing writes
    /// nothing and does not *create* the file: "there is no checkpoint" is how
    /// a drain that never delivered anything looks, and `status` reads it so.
    stored: Checkpoint,
    /// The file `lines` came from, written with every offset so the recorded
    /// identity always describes the file the recorded offset refers to.
    identity: Option<Identity>,
    /// How far into `lines` the transform's loss is charged -- an index, not a
    /// byte offset, so the wake costs one pass over its lines, not one per commit.
    charged: usize,
    /// What this wake added to `discarded_total`, for its own report; the
    /// checkpoint's counter is cumulative across wakes.
    lost: u64,
    writes: u64,
}

impl<'a> Journal<'a> {
    /// `restarted` invalidates every offset the checkpoint carried: the file
    /// they described is gone. Taken here rather than applied by a separate
    /// call, so it cannot land *after* the first commit.
    pub fn new(
        path: PathBuf,
        lines: &'a [Line],
        mut state: Checkpoint,
        identity: Option<Identity>,
        restarted: bool,
    ) -> Self {
        let stored = state.clone();
        if restarted {
            state.restart();
        }
        Self {
            path,
            lines,
            state,
            stored,
            identity,
            charged: 0,
            lost: 0,
            writes: 0,
        }
    }

    pub fn state(&self) -> &Checkpoint {
        &self.state
    }

    /// The refusal table, mutably: evidence gathered mid-pass survives into the
    /// checkpoint whether the pass succeeded or not.
    pub fn quarantine(&mut self) -> &mut Quarantine {
        &mut self.state.quarantine
    }

    /// This wake's contribution to the cumulative `discarded_total`.
    pub fn lost(&self) -> u64 {
        self.lost
    }

    /// How many times this wake rewrote the checkpoint. Exists so the
    /// granularity policy above is measured rather than asserted; nothing in
    /// the shipping path needs it, hence `cfg(test)`.
    #[cfg(test)]
    pub fn writes(&self) -> u64 {
        self.writes
    }

    /// `signal` is delivered to `reached`, `refused` records were given up on
    /// getting there; both are durable when this returns.
    pub fn advance(&mut self, signal: Signal, reached: u64, refused: u64) -> Result<()> {
        if reached > self.state.signal_offset(signal) {
            self.state.advance(signal, reached);
        }
        let lost = refused.saturating_add(self.transform_loss());
        self.state.record_discards(lost)?;
        self.lost = self.lost.saturating_add(lost);
        self.commit()
    }

    /// How the wake ended: `records` delivered, and whether it stopped unable
    /// to resolve the last record in the spool.
    ///
    /// `records > 0` guards the push fields because they describe the *last*
    /// delivery -- zeroing them erases the evidence that a push ever succeeded,
    /// which is what `status` uses to tell a stalled timer from a fresh install.
    /// `held_since_unix` is set once and then left, so `status` can say how long
    /// it has been that way rather than "just now".
    pub fn finished(&mut self, records: u64, stalled: bool, now: u64) {
        if records > 0 {
            self.state.last_push_records = records;
            self.state.last_push_unix = Some(now);
        }
        if stalled {
            self.state.held_since_unix.get_or_insert(now);
        } else {
            self.state.held_since_unix = None;
        }
    }

    /// Writes the checkpoint if anything moved; a no-op otherwise, so callers
    /// may commit defensively.
    pub fn commit(&mut self) -> Result<()> {
        if self.state == self.stored {
            return Ok(());
        }
        // Adopted here rather than at construction so that adopting it is
        // never *itself* a reason to write: a run that exported nothing must
        // not create the checkpoint file.
        self.state.spool.clone_from(&self.identity);
        checkpoint::store(&self.path, &self.state)?;
        self.stored = self.state.clone();
        self.writes = self.writes.saturating_add(1);
        Ok(())
    }

    /// What the transform lost inside the range the shared offset has newly
    /// covered. Only that range is final -- anything past it is re-read next
    /// wake, so counting it here would count it twice.
    fn transform_loss(&mut self) -> u64 {
        let through = self.state.offset;
        let start = self.charged;
        let end = self
            .lines
            .iter()
            .skip(start)
            .position(|line| line.offset >= through);
        let end = end.map_or(self.lines.len(), |found| start.saturating_add(found));
        let Some(newly) = self.lines.get(start..end) else {
            return 0;
        };
        self.charged = end;
        batch::build(newly).counts.discarded()
    }
}
