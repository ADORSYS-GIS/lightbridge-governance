//! Unit tests for `backfill_window` (the pure window math). Split out of the
//! single `backfill.rs` test module (#178); the integration tests live in
//! `tests.rs`/`seats_tests.rs`.

use super::*;
use crate::sync::test_util::date;

/// A recent high-water mark (yesterday) must not shrink the window down
/// to "just the gap" -- the trailing lookback always applies on top of
/// it, per RFC-0001 ("each run re-fetches D-1, D-2 and D-3"). `end` is
/// D-1 (today - 1), never D-0/today: GitHub's Copilot metrics have
/// same-day latency and reject a `today` request outright.
#[test]
fn backfill_window_with_recent_hwm_still_covers_the_trailing_lookback() {
    let today = date("2026-08-10");
    let hwm = Some(date("2026-08-09")); // yesterday
    let (start, end) = backfill_window(hwm, today, 3, 28);
    assert_eq!(start, date("2026-08-07")); // today - 3
    assert_eq!(end, date("2026-08-09")); // today - 1 (D-1), not today
}

/// A stale high-water mark (older than max_backfill_days) must be
/// bounded, not walked back forever. `end` is still D-1, not today.
#[test]
fn backfill_window_with_stale_hwm_is_bounded_by_max_backfill_days() {
    let today = date("2026-08-10");
    let hwm = Some(date("2026-07-01")); // 40 days ago
    let (start, end) = backfill_window(hwm, today, 3, 28);
    assert_eq!(start, date("2026-07-13")); // today - 28
    assert_eq!(end, date("2026-08-09")); // today - 1 (D-1), not today
}

/// No high-water mark at all (first-ever run) is a cold start: bounded
/// by max_backfill_days, not just the trailing lookback. `end` is still
/// D-1, not today.
#[test]
fn backfill_window_with_no_hwm_is_a_cold_start_bounded_by_max_backfill_days() {
    let today = date("2026-08-10");
    let (start, end) = backfill_window(None, today, 3, 28);
    assert_eq!(start, date("2026-07-13")); // today - 28
    assert_eq!(end, date("2026-08-09")); // today - 1 (D-1), not today
}

/// A moderately stale high-water mark (inside max_backfill_days but
/// outside the trailing lookback) must still close the whole gap, not
/// just the trailing window. `end` is still D-1, not today.
#[test]
fn backfill_window_fills_a_gap_wider_than_the_trailing_lookback() {
    let today = date("2026-08-10");
    let hwm = Some(date("2026-07-31")); // 10 days ago
    let (start, end) = backfill_window(hwm, today, 3, 28);
    assert_eq!(start, date("2026-08-01")); // hwm + 1, not just today - 3
    assert_eq!(end, date("2026-08-09")); // today - 1 (D-1), not today
}

/// The window's upper bound is always D-1 (today - 1), never D-0
/// ("today"): GitHub's Copilot metrics have same-day latency and reject
/// a `day=today` request with HTTP 400 every time. Confirmed live in
/// production: before this fix, every 6h scheduled run burned four
/// guaranteed-to-fail requests (one per report type) on `today` and
/// logged a WARN for each, forever. Covers all three `hwm` shapes so a
/// fix that only patches one branch (e.g. only the `None` arm) cannot
/// pass by accident.
#[test]
fn backfill_window_end_is_always_yesterday_never_today() {
    let today = date("2026-08-10");
    let expected_end = date("2026-08-09"); // D-1

    let (_, end_no_hwm) = backfill_window(None, today, 3, 28);
    assert_eq!(end_no_hwm, expected_end, "no hwm: end must be D-1");

    let (_, end_recent_hwm) = backfill_window(Some(date("2026-08-09")), today, 3, 28);
    assert_eq!(end_recent_hwm, expected_end, "recent hwm: end must be D-1");

    let (_, end_stale_hwm) = backfill_window(Some(date("2026-07-01")), today, 3, 28);
    assert_eq!(end_stale_hwm, expected_end, "stale hwm: end must be D-1");
}

/// The exact regression this prevents: a day inside the trailing
/// lookback window (i.e. `today` itself) must never be the window's
/// `end` -- GitHub returns HTTP 400 for `day=today` every single time,
/// so requesting it is always wasted API quota and a guaranteed WARN
/// log, on every 6h run, forever.
#[test]
fn backfill_window_never_places_todays_unqueryable_day_at_the_end() {
    let today = date("2026-08-10");
    for lookback_days in [1, 3, 7] {
        let (_, end) = backfill_window(None, today, lookback_days, 28);
        assert_ne!(
            end, today,
            "lookback_days={lookback_days}: window end must never be today (D-0), \
             GitHub has same-day latency and rejects it with HTTP 400"
        );
    }
}

/// The exact review scenario (BLOCKER 2): a day D fails (or gets an
/// "empty" 204 that later turns out to have been premature), a LATER
/// day still gets a manifest row and pushes the high-water mark past D.
/// Under the old "walk forward from hwm+1" logic, D was never
/// re-attempted again. Under the trailing-window fix, D is still
/// in-window as long as it is within `lookback_days` of "today" --
/// regardless of where the high-water mark now sits.
#[test]
fn backfill_window_still_covers_a_day_the_hwm_has_already_advanced_past() {
    let today = date("2026-08-10");
    let failed_day = date("2026-08-09"); // "D": failed/empty on a prior run
    // A later day (today itself) already has a manifest row, so the
    // high-water mark is now AFTER `failed_day`.
    let hwm = Some(today);
    let (start, end) = backfill_window(hwm, today, 3, 28);
    assert!(
        start <= failed_day && failed_day <= end,
        "expected the window [{start}, {end}] to still include {failed_day}"
    );
}
