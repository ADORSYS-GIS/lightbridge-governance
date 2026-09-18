//! Tests for [`super`]. Split into its own file (issue #175) rather than
//! raising the already-grandfathered LoC ceiling -- the same move
//! `otel/tests.rs` made, applied to the whole `mod tests` block.

use super::*;
use crate::{
    sync::test_util::{date, db_pool, test_config, tmp_archive_dir},
    test_support::{MockGithub, RouteBehavior, SeatsBehavior},
};

/// `run_backfill_at` end to end against a mock GitHub that always
/// succeeds: the day is covered, the window matches `backfill_window`,
/// and (via `run_status`) the high-water mark actually advanced in
/// Postgres. Also covers the seat snapshot: it must succeed alongside
/// the day report and must NOT be counted in `covered`/`window_days`
/// (those track day-based reports only -- see `BackfillOutcome`'s doc
/// comment).
#[tokio::test]
async fn run_backfill_at_covers_a_successful_day_and_advances_the_high_water_mark() {
    let Some(pool) = db_pool().await else {
        return;
    };
    let tenant_id = format!("it-ctl-backfill-ok-{}", std::process::id());
    let org = "it-org-ok";
    let mock = MockGithub::start_with_seats(
        RouteBehavior::AlwaysSucceeds,
        SeatsBehavior::Succeeds { seats: 3 },
        org,
    )
    .await
    .unwrap();
    let client = GithubClient::with_api_base(reqwest::Client::new(), mock.base_url.clone());
    let cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("backfill-ok"),
    );
    let today = chrono::Utc::now().date_naive();

    let result = run_backfill_at(&client, &pool, &cfg, today, None)
        .await
        .unwrap();

    assert_eq!(
        result.window_days, 1,
        "lookback=1, max_backfill=1 => just D-1 (today - 1; GitHub's same-day \
         latency means today, D-0, is never queryable)"
    );
    assert_eq!(result.covered, 1, "covered must count day-reports only");
    assert!(result.failed.is_empty());
    assert_eq!(
        result.seats,
        Ok(3),
        "the seat snapshot must succeed independently and report its row count"
    );
    assert!(result.exit_result().is_ok());
    assert_eq!(
        mock.seats_call_count().unwrap(),
        1,
        "seats must be fetched exactly once per run, not once per report or per day"
    );

    let status = crate::sync::run_status(&pool, &cfg).await.unwrap();
    assert_eq!(
        status,
        crate::sync::SyncStatus::Synced {
            // The manifest row was written for D-1 (today - 1), not
            // today, so the most recent success is 1 day old, not 0.
            age_days: 1,
            unmapped_users: 0
        }
    );
}

/// `run_backfill_at` end to end against a mock GitHub that always
/// fails: the day is NOT covered, no manifest row is written (the
/// high-water mark stays absent), and `exit_result` signals the
/// non-zero exit BLOCKER 1 requires.
#[tokio::test]
async fn run_backfill_at_reports_a_totally_failed_day_and_does_not_advance_the_high_water_mark() {
    let Some(pool) = db_pool().await else {
        return;
    };
    let tenant_id = format!("it-ctl-backfill-fail-{}", std::process::id());
    let org = "it-org-fail";
    let mock = MockGithub::start(RouteBehavior::AlwaysFails(500), org)
        .await
        .unwrap();
    let client = GithubClient::with_api_base(reqwest::Client::new(), mock.base_url.clone());
    let cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("backfill-fail"),
    );
    let today = chrono::Utc::now().date_naive();

    let result = run_backfill_at(&client, &pool, &cfg, today, None)
        .await
        .unwrap();

    assert_eq!(result.window_days, 1);
    assert_eq!(result.covered, 0);
    assert_eq!(result.failed.len(), 1);
    assert!(
        result.exit_result().is_err(),
        "a non-empty window where every day failed must signal a non-zero exit (BLOCKER 1)"
    );

    let status = crate::sync::run_status(&pool, &cfg).await.unwrap();
    assert_eq!(
        status,
        crate::sync::SyncStatus::NeverSynced,
        "a fully failed day must not write a manifest row that would advance the \
         high-water mark"
    );
}

/// The seat snapshot must be fetched exactly once per run even when the
/// backfill window spans several days -- proves it lives outside the
/// per-day loop, not that it merely "happens to" run once in the
/// single-day test above. Broken against a version of `run_backfill_at`
/// that called `ingest_seats` inside the per-day loop: this failed with
/// `seats_call_count() == 3`, not `1`.
#[tokio::test]
async fn seats_are_fetched_once_per_run_even_across_a_multi_day_backfill_window() {
    let Some(pool) = db_pool().await else {
        return;
    };
    let tenant_id = format!("it-ctl-seats-once-{}", std::process::id());
    let org = "it-org-seats-once";
    let mock = MockGithub::start_with_seats(
        RouteBehavior::AlwaysSucceeds,
        SeatsBehavior::Succeeds { seats: 2 },
        org,
    )
    .await
    .unwrap();
    let client = GithubClient::with_api_base(reqwest::Client::new(), mock.base_url.clone());
    let mut cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("seats-once"),
    );
    // A 3-day window (unlike every other test here, which collapses to
    // one day) so a per-day seats bug would show up as call count 3.
    cfg.lookback_days = 3;
    cfg.max_backfill_days = 3;
    let today = chrono::Utc::now().date_naive();

    let result = run_backfill_at(&client, &pool, &cfg, today, None)
        .await
        .unwrap();

    assert_eq!(
        result.window_days, 3,
        "D-1 (today - 1) down through D-3 (today - 3) -- D-0/today is never in the \
         window (same-day latency)"
    );
    assert_eq!(result.covered, 3);
    assert_eq!(result.seats, Ok(2));
    assert_eq!(
        mock.seats_call_count().unwrap(),
        1,
        "seats must be fetched exactly once regardless of how many days the window covers"
    );
}

/// Re-running the same day is the idempotency property RFC-0001 cares
/// about: reprocessing must not change row counts, for seats exactly as
/// much as for the four day-based reports.
#[tokio::test]
async fn seat_snapshot_reprocessing_does_not_change_row_counts() {
    let Some(pool) = db_pool().await else {
        return;
    };
    let tenant_id = format!("it-ctl-seats-idem-{}", std::process::id());
    let org = "it-org-seats-idem";
    let mock = MockGithub::start_with_seats(
        RouteBehavior::AlwaysSucceeds,
        SeatsBehavior::Succeeds { seats: 5 },
        org,
    )
    .await
    .unwrap();
    let client = GithubClient::with_api_base(reqwest::Client::new(), mock.base_url.clone());
    let cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("seats-idem"),
    );
    let today = chrono::Utc::now().date_naive();

    let first = run_backfill_at(&client, &pool, &cfg, today, None)
        .await
        .unwrap();
    let second = run_backfill_at(&client, &pool, &cfg, today, None)
        .await
        .unwrap();

    assert_eq!(first.seats, Ok(5));
    assert_eq!(
        second.seats,
        Ok(5),
        "reprocessing must upsert, not duplicate"
    );

    let (n,): (i64,) = cratestack::sqlx::query_as(
        "SELECT count(*) FROM copilot_seat_snapshots WHERE tenant_id = $1",
    )
    .bind(&tenant_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 5, "re-running the same day must not duplicate seat rows");
}

/// An org with zero Copilot seats must produce `Ok(0)`, not a failure --
/// "no seats" is a real, successfully-queried answer (GitHub returns
/// `200` with `seats: []`), not missing data.
#[tokio::test]
async fn seat_snapshot_on_an_empty_org_succeeds_with_zero_rows() {
    let Some(pool) = db_pool().await else {
        return;
    };
    let tenant_id = format!("it-ctl-seats-empty-{}", std::process::id());
    let org = "it-org-seats-empty";
    let mock = MockGithub::start_with_seats(
        RouteBehavior::AlwaysSucceeds,
        SeatsBehavior::Succeeds { seats: 0 },
        org,
    )
    .await
    .unwrap();
    let client = GithubClient::with_api_base(reqwest::Client::new(), mock.base_url.clone());
    let cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("seats-empty"),
    );
    let today = chrono::Utc::now().date_naive();

    let result = run_backfill_at(&client, &pool, &cfg, today, None)
        .await
        .unwrap();
    assert_eq!(result.seats, Ok(0));
    assert!(result.exit_result().is_ok(), "zero seats is not a failure");
}

/// A seats-only failure (every day-report succeeds) must still fail the
/// run -- RFC-0001's headline use case going silently unfilled is
/// exactly what `exit_result` must not mask just because the unrelated
/// day-based reports were healthy.
#[tokio::test]
async fn a_seats_only_failure_fails_the_run_even_when_every_day_report_succeeded() {
    let Some(pool) = db_pool().await else {
        return;
    };
    let tenant_id = format!("it-ctl-seats-fail-{}", std::process::id());
    let org = "it-org-seats-fail";
    let mock = MockGithub::start_with_seats(
        RouteBehavior::AlwaysSucceeds,
        SeatsBehavior::AlwaysFails(500),
        org,
    )
    .await
    .unwrap();
    let client = GithubClient::with_api_base(reqwest::Client::new(), mock.base_url.clone());
    let cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("seats-fail"),
    );
    let today = chrono::Utc::now().date_naive();

    let result = run_backfill_at(&client, &pool, &cfg, today, None)
        .await
        .unwrap();

    assert_eq!(result.covered, 1, "the day-based reports still succeeded");
    assert!(result.seats.is_err());
    assert!(
        result.exit_result().is_err(),
        "a seats-only failure must still fail the run, not be masked by healthy day-reports"
    );
}

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
