//! Where the durable spool got to: `<state_dir>/otel-daemon-checkpoint.json`,
//! mode `0600`.
//!
//! ## Why this is not `copilot::checkpoint::Checkpoint`
//!
//! Both are an offset into a spool file plus the file's identity, and both are
//! written with the same tmp-then-rename discipline ([`crate::durable_state`]
//! supplies that part to both). Past that they diverge because the two
//! drains have different shapes:
//!
//! - `copilot::checkpoint::Checkpoint` tracks **two** offsets
//!   (`metrics_offset`/`logs_offset`) because one Copilot spool *line* can
//!   produce both a metric and a log record, and the collector can accept one
//!   signal from a wake while refusing the other -- so the two must be able to
//!   diverge. A daemon-spooled entry is already routed to exactly one signal
//!   at receive time ([`super::classify`]); nothing here ever needs to ask
//!   "how far has metrics got, independently of logs", so one `offset` is
//!   enough.
//! - `last_push_records` / `held_since_unix` describe a periodic wake's most
//!   recent run for `status` to report on. The daemon is always-on, not woken
//!   on a timer, so neither field has a caller.
//!
//! Carrying the unused fields anyway would make every reader of this file
//! guess which ones the daemon actually moves. Keeping the shape minimal is
//! the same call [`crate::dashboard`]'s `Surveys` extraction made for a
//! different too-many-fields problem: match the shape to what has a caller.

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    copilot::{quarantine::Quarantine, spool::Identity},
    durable_state,
};

/// The file name under the state directory. Distinct from
/// [`crate::copilot::checkpoint::FILE_NAME`] deliberately: the two checkpoints
/// describe different spools (Copilot's file-exporter outfile vs. the
/// daemon's own durable spool) and must never be read as each other's.
pub const FILE_NAME: &str = "otel-daemon-checkpoint.json";

/// `PartialEq` so a drain pass can compare what it loaded with what it is
/// about to write and skip the write when nothing moved -- mirrors
/// `copilot::checkpoint::Checkpoint` for the same reason: a pass that
/// delivered nothing must not even create this file.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Bytes of the durable spool file already delivered (or given up on --
    /// see `discarded_total`). The next tail starts here.
    #[serde(default)]
    pub offset: u64,
    /// Which file [`Self::offset`] was measured against. `None` only when
    /// there is no file yet, or on a checkpoint written before this field
    /// existed -- not a mismatch; see [`crate::copilot::spool::Identity`].
    #[serde(default)]
    pub spool: Option<Identity>,
    /// Records the drain gave up on -- refused by the collector on their own
    /// across [`crate::copilot::quarantine::REFUSALS_BEFORE_DISCARD`]
    /// separate attempts AND after the collector was shown to accept
    /// something else in the same episode -- see
    /// [`super::drain::advance::advance_one`]'s doc for that second condition. Never
    /// bytes lost to an outage: an unreachable collector leaves the offset
    /// exactly where it was, and the same bytes are retried, not counted
    /// here.
    #[serde(default)]
    pub discarded_total: u64,
    #[serde(default)]
    pub last_discard_unix: Option<u64>,
    /// Records the collector has refused on their own, and on how many
    /// separate attempts -- see [`crate::copilot::quarantine::Quarantine`]'s
    /// module doc for why one refusal is not enough evidence to discard.
    #[serde(default)]
    pub quarantine: Quarantine,
}

impl Checkpoint {
    /// A rotation invalidates the offset: the file it described is gone.
    pub fn restart(&mut self) {
        self.offset = 0;
    }

    pub fn record_discard(&mut self, count: u64) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        self.discarded_total = self.discarded_total.saturating_add(count);
        self.last_discard_unix = Some(now_unix()?);
        Ok(())
    }
}

pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

/// A missing checkpoint means "the daemon has never durably retained
/// anything", the honest starting state. An unreadable one is fatal -- see
/// [`crate::durable_state`]'s module doc for why guessing is unsafe either
/// direction.
pub fn load(path: &Path) -> Result<Checkpoint> {
    durable_state::load(path).context("reading the otel daemon's spool checkpoint")
}

/// Writes tmp-then-rename, `fsync`-durable, so a reader never sees a
/// half-written offset and a crash cannot brick the daemon on restart -- see
/// [`crate::durable_state`]'s module doc.
pub fn store(path: &Path, checkpoint: &Checkpoint) -> Result<()> {
    durable_state::store(path, checkpoint).context("writing the otel daemon's spool checkpoint")
}

pub(super) fn now_unix() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("reading the system clock")?
        .as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_checkpoint_has_zero_offset_and_no_identity() {
        let state = Checkpoint::default();
        assert_eq!(state.offset, 0);
        assert_eq!(state.spool, None);
        assert_eq!(state.discarded_total, 0);
    }

    #[test]
    fn restart_zeroes_the_offset_only() {
        let mut state = Checkpoint {
            offset: 400,
            discarded_total: 3,
            ..Checkpoint::default()
        };
        state.restart();
        assert_eq!(state.offset, 0, "the offset must reset");
        assert_eq!(
            state.discarded_total, 3,
            "a restart is not an amnesty for records already given up on"
        );
    }

    #[test]
    fn record_discard_of_zero_touches_nothing() {
        let mut state = Checkpoint::default();
        state.record_discard(0).expect("zero is always ok");
        assert_eq!(state.discarded_total, 0);
        assert_eq!(
            state.last_discard_unix, None,
            "a no-op must not stamp a time"
        );
    }

    #[test]
    fn record_discard_accumulates_and_stamps_a_time() {
        let mut state = Checkpoint::default();
        state.record_discard(2).expect("record");
        state.record_discard(3).expect("record");
        assert_eq!(state.discarded_total, 5);
        assert!(state.last_discard_unix.is_some());
    }
}
