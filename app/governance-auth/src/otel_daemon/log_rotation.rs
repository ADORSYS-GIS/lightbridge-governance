//! An independent timer that re-checks the log file's size, on top of the
//! one check `crate::logging::init` makes at process start.
//!
//! ## Why this cannot be [`super::drain::pump`]'s job
//!
//! `pump`'s loop only reaches a tick-gated branch on `Pass::Idle` or
//! `Pass::Stopped` -- a pass that uses its full budget (`Pass::BudgetExhausted`)
//! `continue`s immediately, by design (see that module's doc: "a pass that
//! uses the full budget continues immediately... so the boundary does not
//! impose a throughput ceiling"). Under sustained healthy throughput `pump`
//! can chain passes back-to-back indefinitely without ever reaching a tick.
//! Tying the log-size check to that loop would mean the busiest,
//! longest-running case -- exactly the one this exists to bound -- is the
//! one case never checked.
//!
//! Worse, [`super::handle_request`]'s own `trace!` line runs on axum's own
//! request tasks, entirely decoupled from `pump`; nothing about the drain
//! loop bounds what that path writes at all. So this has to be its own
//! timer, checking on its own schedule regardless of what the drain or the
//! receive handler are doing.
//!
//! ## The bound this buys
//!
//! `crate::logging::rotate::MAX_BYTES` plus whatever every log source writes
//! during one [`INTERVAL`]. At the daemon's default `info` level that is at
//! most a handful of failure lines, so the live file settles back to the
//! ~1 MiB / ~4 MiB directory bound `crate::logging::rotate` already
//! advertises -- this time for real, for as long as the daemon runs. At an
//! elevated `GOVERNANCE_AUTH_LOG` (`debug`/`trace`) under real request
//! volume, growth between checks scales with that volume; no fixed interval
//! makes an intentionally verbose, high-traffic daemon bounded in the small,
//! and shortening [`INTERVAL`] only trades that for more frequent `stat`s.
//! Running this daemon at `debug`/`trace` for anything but a short, attended
//! session is the actual unbounded case, and no rotation policy fixes that.

use std::time::Duration;

/// How often to re-check. A `stat(2)` this cheap does not need to run more
/// often than that; matching `drain::PUMP_INTERVAL`'s cadence is a
/// convenience, not a dependency -- the two are free to diverge.
const INTERVAL: Duration = Duration::from_secs(5);

/// Runs until aborted by [`super::serve`], exactly like `drain::pump`.
pub(super) async fn ticker() {
    run(crate::logging::recheck_rotation).await;
}

/// [`ticker`], with the check pulled out as a parameter so a test can count
/// calls instead of touching the real log path.
async fn run(mut check: impl FnMut()) {
    let mut interval = tokio::time::interval(INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        check();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    /// Falsified by tying the check to `drain::pump`'s `Idle`/`Stopped`
    /// branches instead: a fake `pump` that always reports `BudgetExhausted`
    /// (the module doc's "sustained healthy throughput" case) would never
    /// tick, and this would see zero calls instead of several.
    #[tokio::test(start_paused = true)]
    async fn ticks_run_on_a_fixed_schedule_no_matter_what_else_is_happening() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let handle = tokio::spawn(run(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }));

        // Three separate advances, not one big jump: `MissedTickBehavior::Delay`
        // collapses ticks it was not polled in time for into a single one
        // (that is the point of `Delay` -- no burst catch-up), so advancing
        // 3 intervals in one leap and yielding once would also observe 1
        // call and pass for the wrong reason. Virtual time under
        // `start_paused` rather than a real sleep either way.
        for _ in 0..3 {
            tokio::time::advance(INTERVAL).await;
            tokio::task::yield_now().await;
        }

        assert!(
            calls.load(Ordering::SeqCst) >= 3,
            "expected at least 3 checks after 3 intervals, got {}",
            calls.load(Ordering::SeqCst)
        );

        handle.abort();
    }
}
