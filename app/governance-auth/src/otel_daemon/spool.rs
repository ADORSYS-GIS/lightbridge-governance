//! The in-memory spool: retains payloads on a refused mint or an unreachable
//! collector, never drops.
//!
//! ## Why in-memory, and why that is acceptable here
//!
//! #268 explicitly allows an in-memory buffer "provided the story says so
//! plainly and #S2 lands before the profile is switched on by default." State
//! it plainly: **on process exit the buffer is lost.** That is accepted for
//! #268 only because durability (#S2) is a separate story that must land
//! before the daemon becomes the default profile.
//!
//! ## Cap, not growth
//!
//! [`CAPACITY`] bounds total retained bytes, mirroring [`crate::copilot`]'s
//! `MAX_READ` per sweep times two — headroom for a burst without the #230
//! unbounded-growth failure. At capacity, [`Spool::retain`] **refuses** rather
//! than dropping the oldest: the unavailable branch is the restrictive branch
//! (ADR/AGENTS.md), so a full spool costs latency, never data.
//!
//! ## Why each entry carries its `Signal`
//!
//! A retry must re-poster to the same path the original forward used. For a
//! non-JSON (protobuf) body the URL path is the only thing that names the signal
//! ([`super::classify`] falls back to it), and a retained payload has no path of
//! its own once it sits in the spool — so the signal is captured at retain time
//! and stored alongside the bytes.

use std::collections::VecDeque;

use anyhow::{Result, bail};

use crate::copilot::Signal;

/// Total bytes the in-memory spool will retain. 16 MiB — twice
/// [`crate::copilot::spool::MAX_READ`], so a wake's worth of both signals fits
/// with room for a burst, without unbounded growth.
pub const CAPACITY: usize = 16 * 1024 * 1024;

/// A bounded FIFO of `(signal, payload)` pairs.
#[derive(Default)]
pub struct Spool {
    buffer: VecDeque<(Signal, Vec<u8>)>,
    total_bytes: usize,
}

impl Spool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Retains a payload and the signal it was routed on, refusing (not
    /// silently dropping) when at capacity.
    ///
    /// The refusal is returned to the caller so it can be surfaced loudly —
    /// never swallowed into "accepted" with no record. Within the daemon this
    /// already counted as a loss report; the caller decides what to do with
    /// the `Err`.
    pub fn retain(&mut self, signal: Signal, payload: Vec<u8>) -> Result<()> {
        let len = payload.len();
        if self.total_bytes.saturating_add(len) > CAPACITY {
            bail!(
                "daemon spool full ({total} of {CAPACITY} bytes retained); the collector may be \
                 unreachable — retry later",
                total = self.total_bytes,
            );
        }
        self.buffer.push_back((signal, payload));
        self.total_bytes = self.total_bytes.saturating_add(len);
        Ok(())
    }

    /// Pops the oldest retained `(signal, payload)`, for a retry. `None` when
    /// empty.
    pub fn drain_one(&mut self) -> Option<(Signal, Vec<u8>)> {
        let (signal, payload) = self.buffer.pop_front()?;
        self.total_bytes = self.total_bytes.saturating_sub(payload.len());
        Some((signal, payload))
    }

    /// Total bytes currently retained.
    pub fn pending(&self) -> usize {
        self.total_bytes
    }

    /// Number of payloads currently retained. Used by tests now and by the
    /// #S4 health/status story later.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Spool observability for #S4 (daemon health/status); used in tests today"
        )
    )]
    pub fn pending_count(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retain_and_drain_is_fifo() {
        let mut spool = Spool::new();
        spool.retain(Signal::Logs, b"one".to_vec()).expect("retain");
        spool
            .retain(Signal::Metrics, b"two".to_vec())
            .expect("retain");
        spool
            .retain(Signal::Logs, b"three".to_vec())
            .expect("retain");
        assert_eq!(spool.pending_count(), 3);
        assert_eq!(
            spool.drain_one(),
            Some((Signal::Logs, b"one".to_vec())),
            "the signal must be retained with its payload"
        );
        assert_eq!(spool.drain_one(), Some((Signal::Metrics, b"two".to_vec())));
        assert_eq!(spool.drain_one(), Some((Signal::Logs, b"three".to_vec())));
        assert_eq!(spool.drain_one(), None);
        assert_eq!(spool.pending(), 0);
    }

    #[test]
    fn pending_tracks_total_bytes() {
        let mut spool = Spool::new();
        spool.retain(Signal::Logs, vec![0u8; 5]).expect("retain");
        spool.retain(Signal::Metrics, vec![0u8; 7]).expect("retain");
        assert_eq!(spool.pending(), 12);
        spool.drain_one();
        assert_eq!(spool.pending(), 7);
    }

    #[test]
    fn at_capacity_retain_refuses_rather_than_dropping() {
        let mut spool = Spool::new();
        spool
            .retain(Signal::Logs, vec![0u8; CAPACITY])
            .expect("fill exactly");
        // One more byte must refuse, not evict the oldest.
        let error = spool
            .retain(Signal::Metrics, vec![0u8; 1])
            .expect_err("a payload over capacity must refuse");
        assert!(
            format!("{error:#}").contains("spool full"),
            "names the condition: {error:#}"
        );
        assert_eq!(spool.pending_count(), 1, "nothing may be evicted");
    }
}
