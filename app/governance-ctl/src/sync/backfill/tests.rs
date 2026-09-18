//! Integration tests for `run_backfill_at`'s day-report path against a mock
//! GitHub and real Postgres. Split out of the single `backfill.rs` test
//! module (#178); the seat-snapshot integration tests live in
//! `seats_tests.rs`, the pure window/outcome tests in `window_tests.rs` and
//! `outcome_tests.rs`.

use super::*;
use crate::{
    sync::test_util::{db_pool, test_config, tmp_archive_dir},
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
