//! The outcome of one `sync` (backfill) run and the exit-code decision.

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

#[cfg(test)]
mod tests {
    use super::*;

    /// `seats` defaults to `Ok(0)` -- a healthy, empty seat snapshot --
    /// so every existing "day-report" `exit_result` scenario below stays
    /// about exactly what it was testing before `seats` existed. The two
    /// seats-specific tests override it explicitly.
    fn outcome(covered: usize, window_days: usize, failed: &[&str]) -> BackfillOutcome {
        BackfillOutcome {
            outcomes: Vec::new(),
            covered,
            failed: failed
                .iter()
                .map(|d| (d.to_string(), "boom".to_owned()))
                .collect(),
            window_days,
            seats: Ok(0),
        }
    }

    /// BLOCKER 1: a non-empty window where every day failed must produce an
    /// `Err`, so `main` propagates it and the process exits non-zero.
    #[test]
    fn exit_result_errors_when_every_day_in_a_nonempty_window_failed() {
        let result = outcome(
            0,
            4,
            &["2026-08-07", "2026-08-08", "2026-08-09", "2026-08-10"],
        );
        assert!(result.exit_result().is_err());
    }

    /// A partial failure (some days ok) must stay exit 0 -- it is logged
    /// loudly by `run_backfill_at` already, and the failed day is
    /// re-attempted by the next run's trailing window (BLOCKER 2), so
    /// failing the whole job here would only be noise.
    #[test]
    fn exit_result_is_ok_on_a_partial_failure() {
        let result = outcome(3, 4, &["2026-08-07"]);
        assert!(result.exit_result().is_ok());
    }

    /// An empty window (already caught up; nothing to do) must stay exit 0
    /// -- `covered == 0` here means "there was nothing to cover", not "every
    /// day failed".
    #[test]
    fn exit_result_is_ok_on_an_empty_window() {
        let result = outcome(0, 0, &[]);
        assert!(result.exit_result().is_ok());
    }

    /// A fully successful window must stay exit 0.
    #[test]
    fn exit_result_is_ok_when_every_day_succeeded() {
        let result = outcome(4, 4, &[]);
        assert!(result.exit_result().is_ok());
    }

    /// A seats failure alone (every day-report otherwise healthy) must
    /// still fail the run: `covered`/`window_days` say "the day-based
    /// reports were fine", but `seats` failing must not be masked by that.
    /// Broken against a version of `exit_result` that only checked
    /// `covered == 0`: this failed with `exit_result().is_ok() == true`,
    /// silently swallowing the seats failure.
    #[test]
    fn exit_result_errors_on_a_seats_only_failure_even_with_a_fully_healthy_window() {
        let mut result = outcome(4, 4, &[]);
        result.seats = Err("boom".to_owned());
        assert!(
            result.exit_result().is_err(),
            "a seats failure must fail the run even when every day-report succeeded"
        );
    }

    /// The reverse of the case above: a totally failed day-report window
    /// AND a failed seats snapshot must still produce exactly one `Err`
    /// (not panic combining the two), so the double-failure path is
    /// exercised too, not just each failure mode in isolation.
    #[test]
    fn exit_result_errors_when_both_reports_and_seats_fail() {
        let mut result = outcome(0, 4, &["2026-08-07"]);
        result.seats = Err("boom".to_owned());
        assert!(result.exit_result().is_err());
    }

    /// A healthy seats snapshot must not, by itself, paper over the
    /// existing "every day failed" failure mode -- proves the two checks in
    /// `exit_result` are independent, not "seats overrides reports".
    #[test]
    fn exit_result_still_errors_on_a_failed_window_even_with_healthy_seats() {
        let result = outcome(0, 4, &["2026-08-07"]);
        assert_eq!(result.seats, Ok(0));
        assert!(result.exit_result().is_err());
    }
}
