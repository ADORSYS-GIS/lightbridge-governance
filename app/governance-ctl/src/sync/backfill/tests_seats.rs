//! Integration tests for the once-per-run seat snapshot path of
//! `run_backfill_at`, against a mock GitHub and a real Postgres.

use super::*;
use crate::{
    sync::test_util::{db_pool, test_config, tmp_archive_dir},
    test_support::{MockGithub, RouteBehavior, SeatsBehavior},
};

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
