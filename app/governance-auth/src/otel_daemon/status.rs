//! What `status` needs to know about this daemon's own durable spool,
//! gathered without a network call -- the daemon-side analogue of
//! `copilot::status::SpoolStatus`.
//!
//! Not the same shape, because the two spools are not the same shape --
//! `checkpoint::Checkpoint`'s own module doc explains why in detail, and the
//! summary is: Copilot's checkpoint tracks two offsets and a single
//! `held_since_unix` (the drain can only ever be stuck on the *last* record in
//! the file); this daemon's checkpoint tracks one offset and a whole
//! [`crate::copilot::quarantine::Quarantine`] table, because
//! `otel_daemon::drain::lookahead::walk` can hold on any record, not only the
//! last one, once a run of consecutive refusals exceeds `MAX_LOOKAHEAD`. See
//! `docs/runbooks/otel-daemon-wedged.md` for the failure this exists to make
//! visible, and why one `status` call cannot, by itself, tell "still retrying,
//! will clear on its own" from "wedged, needs `scripts/otel-daemon-unstick.py`"
//! -- both look identical at a single point in time; only the runbook's
//! "check twice, minutes apart" distinguishes them. This module's job is to
//! put the numbers that comparison needs in one place, not to make the call
//! itself.

use std::path::PathBuf;

use super::{checkpoint, spool};
use crate::cache;

pub struct DaemonSpoolStatus {
    pub path: PathBuf,
    /// `None` when the spool file does not exist yet -- the daemon creates it
    /// on first receive, so this is the ordinary state right after install,
    /// not an error.
    pub size: Option<u64>,
    pub offset: u64,
    /// Bytes written but not yet delivered. Saturating, so a spool that
    /// shrank under a stale checkpoint (a rotation, or a manual edit) reads 0
    /// rather than underflowing.
    pub pending: u64,
    /// Records the drain gave up on for good -- refused enough separate times
    /// AND proven against a later, accepted record. Real, permanent loss, not
    /// a stall; see `checkpoint::Checkpoint::discarded_total`'s own doc.
    pub discarded_total: u64,
    pub last_discard_unix: Option<u64>,
    /// The checkpoint file could not be read. Distinct from "no checkpoint
    /// yet": one is a fresh install, the other is a drain failing on every
    /// attempt and otherwise indistinguishable from it.
    pub checkpoint_unreadable: bool,
    /// What is currently held pending a later probe proving the collector --
    /// `(how many records, the worst one's refusal count, when it was last
    /// refused)`. `None` is the ordinary state. See
    /// `otel_daemon::drain::lookahead`'s module doc, and `Quarantine::held`'s
    /// own doc for why this is one field, not two.
    pub held: Option<(usize, u32, u64)>,
}

impl DaemonSpoolStatus {
    /// `None` only when the state directory cannot be resolved at all -- in
    /// which case `status` shows no row rather than one full of guesses,
    /// mirroring `copilot::status::SpoolStatus::survey`.
    pub fn survey() -> Option<Self> {
        let state_dir = cache::state_dir().ok()?;
        let path = state_dir.join(spool::FILE_NAME);
        let size = std::fs::metadata(&path).ok().map(|metadata| metadata.len());

        let checkpoint_path = checkpoint::path(&state_dir);
        let (state, checkpoint_unreadable) = match checkpoint::load(&checkpoint_path) {
            Ok(state) => (state, false),
            Err(_) => (checkpoint::Checkpoint::default(), true),
        };

        Some(Self {
            path,
            size,
            offset: state.offset,
            pending: size.unwrap_or_default().saturating_sub(state.offset),
            discarded_total: state.discarded_total,
            last_discard_unix: state.last_discard_unix,
            checkpoint_unreadable,
            held: state.quarantine.held(),
        })
    }

    /// Whether the spool is present at all, from this command's point of
    /// view: a file that has never existed means the daemon has never
    /// received anything, which is a different row from "receiving and
    /// stuck".
    pub fn present(&self) -> bool {
        self.size.is_some()
    }
}
