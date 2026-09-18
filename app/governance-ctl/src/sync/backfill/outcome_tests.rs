//! Unit tests for `BackfillOutcome::exit_result`. Split out of the single
//! `backfill.rs` test module (#178); the integration tests live in
//! `tests.rs`/`seats_tests.rs`.

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
