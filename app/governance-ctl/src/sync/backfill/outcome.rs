//! The outcome model of one `sync` (backfill) run and the decision of whether
//! it should exit non-zero. Split out of `backfill.rs` (#178); the decision
//! logic is unit-tested in `outcome_tests.rs`.

use anyhow::Result;

/// The outcome of one `sync` (backfill) run.
#[derive(Debug, Clone)]
pub struct BackfillOutcome {
    /// Per-report outcomes for every day that ingested without error, plus
    /// the seat snapshot's own outcome when it succeeded (report type
    /// `SEATS_REPORT_TYPE`) -- so it shows up in the same
    /// `governance.copilot.reports`/`rows` push metrics as the day-based
    /// reports, without a second metric family.
    pub outcomes: Vec<governance_copilot::ReportOutcome>,
    /// Days in the window that ingested without error. An "empty" (204)
    /// report still counts as covered -- only a transport/auth/parse/storage
    /// error for the day counts as failed. Never incremented for the seat
    /// snapshot -- see `seats` below.
    pub covered: usize,
    /// Days in the window that errored: the day plus the error's rendered
    /// message (not the `anyhow::Error` itself -- `main` only needs to log
    /// and count these, and error types here are not `Clone`).
    pub failed: Vec<(String, String)>,
    /// Total days the computed window covered. Lets the caller distinguish
    /// "nothing to do" (window empty; fine) from "everything in a non-empty
    /// window failed" (must exit non-zero -- see `main`'s `Command::Sync`).
    pub window_days: usize,
    /// Outcome of the once-per-run seat snapshot (RFC-0001's headline use
    /// case: "who has a seat and has never used it"). `Ok(n)` = `n` seat
    /// rows upserted (`0` is a real "org has no seats" answer, not a
    /// failure). Tracked independently of `covered`/`failed`/`window_days`
    /// on purpose: seats and the day-based reports are different failure
    /// domains on a different axis entirely (once-per-run vs.
    /// once-per-day), so one failing must neither mask nor be masked by the
    /// other -- see `exit_result` below, which fails the run on either.
    pub seats: Result<usize, String>,
}

impl BackfillOutcome {
    /// Whether a `Command::Sync` run should exit non-zero: either the
    /// window was non-empty and every day in it failed (BLOCKER 1 from the
    /// pre-go-live review -- a totally broken run, dead credential, GitHub
    /// unreachable), or the once-per-run seat snapshot itself failed. Both
    /// are checked independently and either alone is sufficient to fail the
    /// run -- a healthy seat snapshot must not paper over every report
    /// failing, and healthy reports must not paper over a broken seat
    /// snapshot (RFC-0001's headline use case going silently unfilled is
    /// exactly the kind of failure `exit_result` exists to surface, not
    /// mask).
    ///
    /// Returns `Ok(())` only when both are healthy: nothing to do in an
    /// empty window (or a partial day failure, logged loudly by
    /// `run_backfill_at` already and re-attempted by the next run's
    /// trailing window -- BLOCKER 2) AND the seat snapshot succeeded.
    pub fn exit_result(&self) -> Result<()> {
        let reports_failed = self.window_days > 0 && self.covered == 0;
        let seats_failed = self.seats.is_err();
        match (reports_failed, seats_failed) {
            (true, true) => anyhow::bail!(
                "backfill covered 0 of {} day(s) in the window AND the seat snapshot failed \
                 (first report failure: {:?}; seats failure: {:?})",
                self.window_days,
                self.failed.first(),
                self.seats.as_ref().err()
            ),
            (true, false) => anyhow::bail!(
                "backfill covered 0 of {} day(s) in the window; every day failed \
                 (first failure: {:?})",
                self.window_days,
                self.failed.first()
            ),
            (false, true) => anyhow::bail!(
                "the once-per-run Copilot seat snapshot failed: {:?}",
                self.seats.as_ref().err()
            ),
            (false, false) => Ok(()),
        }
    }
}
