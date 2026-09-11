//! An independent timer that compacts the durable spool -- on top of
//! [`spool::commit::try_reclaim`]'s per-commit truncate -- so a dead prefix
//! cannot accumulate forever under continuous traffic that never presents an
//! exact "fully caught up" instant. See `spool::compact`'s own doc for the
//! gap this closes and why the rewrite it uses is safe for this particular
//! file.
//!
//! Same reasoning as [`super::log_rotation`] for why this needs its own
//! timer rather than piggybacking on [`super::drain::pump`]: `pump` skips
//! its tick entirely under sustained throughput (`Pass::BudgetExhausted`
//! `continue`s immediately), which is exactly the condition that produces a
//! growing dead prefix in the first place. Tying this check to `pump`'s tick
//! would mean the one case it exists to catch is the one case it would never
//! run for.
//!
//! A slower cadence than `log_rotation`'s: `compact_if_stuck` reads and
//! rewrites the pending tail (up to [`super::spool::CAPACITY`], 16 MiB),
//! against `log_rotation`'s `stat`-only check. 30s keeps that cost off the
//! critical path without leaving a stuck dead prefix growing for long.

use std::time::Duration;

use super::DaemonState;

const INTERVAL: Duration = Duration::from_secs(30);

/// Runs until aborted by [`super::serve`], exactly like `drain::pump` and
/// `log_rotation::ticker`.
pub(super) async fn ticker(state: DaemonState) {
    let mut interval = tokio::time::interval(INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        // Best-effort: an error here is disk hygiene deferred, never a
        // reason to stop the daemon. `with_spool` already keeps this off
        // the tokio worker thread (P2-7) and resumes a genuine panic rather
        // than swallowing it.
        let _ = super::drain::with_spool(&state, |spool| spool.compact_if_stuck()).await;
    }
}
