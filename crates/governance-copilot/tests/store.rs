//! Integration tests for the Copilot connector's Postgres persistence.
//!
//! Requires `DATABASE_URL` (see `just up`); skips with an explicit message
//! otherwise. Each test uses a unique tenant/org/day so concurrent runs do not
//! collide, and asserts the central RFC-0001 invariant: reprocessing the same
//! natural key upserts in place and never grows row counts (idempotency).

use chrono::{TimeZone, Utc};
use governance_copilot::{
    OrgDaily, RepoDaily, SeatSnapshot, UserDaily, UserTeam, high_water_mark, parse_seats,
    replay_report, unmapped_user_count, upsert_manifest, upsert_org_daily, upsert_repo_daily,
    upsert_seat_snapshot, upsert_user_daily, upsert_user_team, verify_manifests,
};
use governance_core::MicroUsd;

static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

fn tenant() -> String {
    "it-copilot".to_owned()
}

#[expect(
    clippy::expect_used,
    reason = "test fixture helper, not a #[test] fn, so clippy's test carve-out in clippy.toml does \
              not cover it; a failure here means the test setup broke, not the code under test"
)]
async fn pool() -> Option<cratestack::sqlx::PgPool> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(v) => v,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set (governance-copilot integration)");
            return None;
        }
    };
    let pool = cratestack::sqlx::PgPool::connect(&database_url)
        .await
        .expect("connect");
    MIGRATED
        .get_or_init(|| async {
            governance_core::migrate::run(&pool).await.expect("migrate");
        })
        .await;
    Some(pool)
}

#[expect(clippy::expect_used, reason = "test fixture helper; see pool()")]
async fn count(pool: &cratestack::sqlx::PgPool, table: &str, tenant_id: &str) -> i64 {
    let q = format!("SELECT count(*) FROM {table} WHERE tenant_id = $1");
    let row: (i64,) = cratestack::sqlx::query_as(&q)
        .bind(tenant_id)
        .fetch_one(pool)
        .await
        .expect("count");
    row.0
}

#[tokio::test]
async fn org_daily_reprocessing_never_duplicates_rows() {
    let Some(pool) = pool().await else { return };
    let t = tenant();
    let org = "it-org-1";

    let rows = vec![OrgDaily {
        organization_id: org.to_owned(),
        report_day: "2026-08-01".to_owned(),
        active_users: 10,
        engaged_users: 4,
        total_interactions: 150,
        total_completions: 120,
        ai_credits: 0,
        net_cost_micro_usd: MicroUsd(0),
    }];

    // First run inserts.
    let inserted = upsert_org_daily(&pool, &t, &rows).await.unwrap();
    assert_eq!(inserted, 1);
    assert_eq!(count(&pool, "copilot_org_dailys", &t).await, 1);

    // Reprocess the same (org, day) with a changed value; it must upsert, not
    // insert a second row.
    let rows2 = vec![OrgDaily {
        active_users: 11,
        ..rows[0].clone()
    }];
    let _ = upsert_org_daily(&pool, &t, &rows2).await.unwrap();
    assert_eq!(count(&pool, "copilot_org_dailys", &t).await, 1);
}

#[tokio::test]
async fn user_and_team_upserts_are_idempotent() {
    let Some(pool) = pool().await else { return };
    let t = tenant();
    let org = "it-org-2";

    let users = vec![UserDaily {
        provider_user_id: "1001".to_owned(),
        user_login: "octocat".to_owned(),
        report_day: "2026-08-02".to_owned(),
        total_interactions: 42,
        total_completions: 20,
        ai_credits: 2,
        net_cost_micro_usd: MicroUsd(20_000),
    }];
    upsert_user_daily(&pool, &t, org, &users).await.unwrap();
    upsert_user_daily(&pool, &t, org, &users).await.unwrap();
    assert_eq!(count(&pool, "copilot_user_dailys", &t).await, 1);

    let teams = vec![UserTeam {
        user_id: "1001".to_owned(),
        team_id: "9001".to_owned(),
        team_slug: "eng-platform".to_owned(),
        report_day: "2026-08-02".to_owned(),
    }];
    upsert_user_team(&pool, &t, org, &teams).await.unwrap();
    upsert_user_team(&pool, &t, org, &teams).await.unwrap();
    assert_eq!(count(&pool, "copilot_user_teams", &t).await, 1);

    let repos = vec![RepoDaily {
        repository_id: "r1".to_owned(),
        report_day: "2026-08-02".to_owned(),
        coding_agent_activity: 3,
        code_review_activity: 2,
        pull_request_activity: 1,
    }];
    upsert_repo_daily(&pool, &t, org, &repos).await.unwrap();
    upsert_repo_daily(&pool, &t, org, &repos).await.unwrap();
    assert_eq!(count(&pool, "copilot_repo_dailys", &t).await, 1);
}

#[tokio::test]
async fn manifest_drives_the_high_water_mark() {
    let Some(pool) = pool().await else { return };
    // Deliberately a distinct tenant so this test's watermark is isolated.
    let t = format!("it-manifest-{}", std::process::id());

    upsert_manifest(
        &pool,
        &t,
        "github_copilot",
        "g1",
        "organization-1-day",
        "2026-08-03",
        "ok",
        5,
    )
    .await
    .unwrap();
    upsert_manifest(
        &pool,
        &t,
        "github_copilot",
        "g1",
        "organization-1-day",
        "2026-08-04",
        "ok",
        8,
    )
    .await
    .unwrap();

    let hwm = high_water_mark(&pool, &t, "github_copilot").await.unwrap();
    assert_eq!(hwm.map(|d| d.to_string()), Some("2026-08-04".to_owned()));
}

/// A tenant with no manifests yet must read as `None`, not error: MAX() over
/// an empty table returns a single NULL row, and decoding that as a plain
/// `NaiveDate` used to fail the very first `status`/`sync` on a fresh tenant.
#[tokio::test]
async fn high_water_mark_on_empty_tenant_is_none() {
    let Some(pool) = pool().await else { return };
    let t = format!("it-empty-hwm-{}", std::process::id());

    let hwm = high_water_mark(&pool, &t, "github_copilot").await.unwrap();
    assert_eq!(hwm, None);
}

/// Replaying the same raw payload twice must not grow any row count: replay
/// shares `replay_report` with live ingestion, so this proves the RFC-0001
/// recovery path is as idempotent as the live one.
#[tokio::test]
async fn replay_report_is_idempotent() {
    let Some(pool) = pool().await else { return };
    let t = format!("it-replay-{}", std::process::id());
    let org = "it-replay-org";

    let ndjson = concat!(
        "{\"day\":\"2026-08-05\",\"organization_id\":\"g9\",",
        "\"total_active_users\":10,\"total_engaged_users\":4,",
        "\"total_completions\":120,\"total_chat_engagements\":30}\n",
        "{\"day\":\"2026-08-05\",\"user_id\":\"1001\",\"user_login\":\"octocat\",",
        "\"total_engagements\":42,\"total_completions\":20,\"ai_credits\":2.5}\n",
    );

    let org_rows = replay_report(
        &pool,
        &t,
        org,
        "2026-08-05",
        "organization-1-day",
        ndjson.as_bytes(),
    )
    .await
    .unwrap();
    let user_rows = replay_report(
        &pool,
        &t,
        org,
        "2026-08-05",
        "users-1-day",
        ndjson.as_bytes(),
    )
    .await
    .unwrap();
    assert_eq!(org_rows, 1);
    assert_eq!(user_rows, 1);

    // Replay the identical bytes; counts must not move.
    let _ = replay_report(
        &pool,
        &t,
        org,
        "2026-08-05",
        "organization-1-day",
        ndjson.as_bytes(),
    )
    .await
    .unwrap();
    let _ = replay_report(
        &pool,
        &t,
        org,
        "2026-08-05",
        "users-1-day",
        ndjson.as_bytes(),
    )
    .await
    .unwrap();
    assert_eq!(count(&pool, "copilot_org_dailys", &t).await, 1);
    assert_eq!(count(&pool, "copilot_user_dailys", &t).await, 1);

    // A manifest records what the replay covered; the high-water mark follows.
    let hwm = high_water_mark(&pool, &t, "github_copilot").await.unwrap();
    assert_eq!(hwm.map(|d| d.to_string()), Some("2026-08-05".to_owned()));
}

/// `verify_manifests` must agree with the stored rows after a clean replay,
/// and must name a day the moment one of its rows is deleted.
#[tokio::test]
async fn verify_detects_drift_between_manifest_and_rows() {
    let Some(pool) = pool().await else { return };
    let t = format!("it-verify-{}", std::process::id());
    let org = "it-verify-org";

    let ndjson = concat!(
        "{\"day\":\"2026-08-06\",\"user_id\":\"1001\",\"user_login\":\"octocat\",",
        "\"total_engagements\":42,\"total_completions\":20,\"ai_credits\":2.5}\n",
        "{\"day\":\"2026-08-06\",\"user_id\":\"1002\",\"user_login\":\"hubot\",",
        "\"total_engagements\":7,\"total_completions\":3,\"ai_credits\":0.5}\n",
    );
    let n = replay_report(
        &pool,
        &t,
        org,
        "2026-08-06",
        "users-1-day",
        ndjson.as_bytes(),
    )
    .await
    .unwrap();
    assert_eq!(n, 2);

    // Clean state: no drift.
    let drift = verify_manifests(&pool, &t, "github_copilot", org)
        .await
        .unwrap();
    assert!(
        drift.is_empty(),
        "clean ingest must verify clean: {drift:?}"
    );

    // A lost row (simulated partial deletion) must surface as drift.
    cratestack::sqlx::query(
        "DELETE FROM copilot_user_dailys \
         WHERE tenant_id = $1 AND organization_id = $2 AND report_day = CAST('2026-08-06' AS date) \
           AND provider_user_id = '1002'",
    )
    .bind(&t)
    .bind(org)
    .execute(&pool)
    .await
    .unwrap();

    let drift = verify_manifests(&pool, &t, "github_copilot", org)
        .await
        .unwrap();
    assert_eq!(
        drift.len(),
        1,
        "deleted row must surface as drift: {drift:?}"
    );
    assert_eq!(drift[0].report, "users-1-day");
    assert_eq!(drift[0].expected, 2);
    assert_eq!(drift[0].actual, 1);

    // Team attribution: one user has usage but no team row -> unmapped.
    let unmapped = unmapped_user_count(&pool, &t, org, "2026-08-06")
        .await
        .unwrap();
    assert_eq!(unmapped, 1);
}

/// The RFC-0001 idempotency property applied to seats: reprocessing the
/// same snapshot day must upsert in place, never duplicate.
#[tokio::test]
async fn seat_snapshot_reprocessing_never_duplicates_rows() {
    let Some(pool) = pool().await else { return };
    let t = format!("it-seats-{}", std::process::id());
    let org = "it-org-seats";

    let rows = vec![SeatSnapshot {
        provider_user_id: "9001".to_owned(),
        user_login: "octocat".to_owned(),
        snapshot_day: "2026-08-07".to_owned(),
        seat_assigned_at: None,
        last_activity_at: None,
        last_activity_editor: None,
        seat_state: "active".to_owned(),
    }];

    let inserted = upsert_seat_snapshot(&pool, &t, org, &rows).await.unwrap();
    assert_eq!(inserted, 1);
    assert_eq!(count(&pool, "copilot_seat_snapshots", &t).await, 1);

    // Reprocess the same (org, snapshot_day, provider_user_id) with a
    // changed field; it must upsert, not insert a second row.
    let rows2 = vec![SeatSnapshot {
        seat_state: "pending_cancellation".to_owned(),
        ..rows[0].clone()
    }];
    let _ = upsert_seat_snapshot(&pool, &t, org, &rows2).await.unwrap();
    assert_eq!(count(&pool, "copilot_seat_snapshots", &t).await, 1);
}

/// A seat with no recorded activity (never used -- RFC-0001's exact
/// motivating question) must be stored as SQL `NULL`, not a fabricated
/// value that would read as "known" downstream.
#[tokio::test]
async fn seat_snapshot_with_no_activity_stores_null_not_a_default() {
    let Some(pool) = pool().await else { return };
    let t = format!("it-seats-null-{}", std::process::id());
    let org = "it-org-seats-null";

    let rows = vec![SeatSnapshot {
        provider_user_id: "9002".to_owned(),
        user_login: "neveruser".to_owned(),
        snapshot_day: "2026-08-07".to_owned(),
        seat_assigned_at: Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
        last_activity_at: None,
        last_activity_editor: None,
        seat_state: "active".to_owned(),
    }];
    upsert_seat_snapshot(&pool, &t, org, &rows).await.unwrap();

    let (last_activity_at, last_activity_editor): (Option<chrono::DateTime<Utc>>, Option<String>) =
        cratestack::sqlx::query_as(
            "SELECT last_activity_at, last_activity_editor FROM copilot_seat_snapshots \
             WHERE tenant_id = $1 AND provider_user_id = '9002'",
        )
        .bind(&t)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        last_activity_at, None,
        "a never-used seat must store NULL, not a fabricated timestamp"
    );
    assert_eq!(last_activity_editor, None);
}

/// The seat snapshot's manifest row must NOT advance the high-water mark
/// the four day-based reports use for `backfill_window`'s gap-filling.
/// GitHub's `/copilot/billing/seats` always reflects "today", so if this
/// were included, a seat snapshot succeeding every run would make the
/// day-based reports look perpetually current even while every one of them
/// has been failing for a week -- see `store::high_water_mark`'s doc
/// comment. This test writes ONLY a seats manifest row (no day-report
/// manifest at all) and asserts the high-water mark still reads as `None`.
#[tokio::test]
async fn a_seats_only_manifest_does_not_become_the_daily_reports_high_water_mark() {
    let Some(pool) = pool().await else { return };
    let t = format!("it-seats-hwm-{}", std::process::id());
    let org = "it-org-seats-hwm";

    upsert_manifest(
        &pool,
        &t,
        "github_copilot",
        org,
        governance_copilot::SEATS_REPORT_TYPE,
        "2026-08-07",
        "ok",
        3,
    )
    .await
    .unwrap();

    let hwm = high_water_mark(&pool, &t, "github_copilot").await.unwrap();
    assert_eq!(
        hwm, None,
        "a seats-only manifest must not be read as the day-reports' high-water mark"
    );
}

/// `verify_manifests` must reconcile a seat-snapshot manifest row against
/// `copilot_seat_snapshots` (not error on an "unknown report type") --
/// clean on a matching snapshot, and it must name the day the moment a seat
/// row goes missing, exactly like the four day-based reports above.
#[tokio::test]
async fn verify_manifests_reconciles_a_seat_snapshot() {
    let Some(pool) = pool().await else { return };
    let t = format!("it-seats-verify-{}", std::process::id());
    let org = "it-org-seats-verify";
    let day = "2026-08-07";

    let page = concat!(
        r#"{"seats":["#,
        r#"{"assignee":{"id":1,"login":"a"}},"#,
        r#"{"assignee":{"id":2,"login":"b"}}"#,
        r#"]}"#
    );
    let rows = parse_seats(
        format!("[{page}]").as_bytes(),
        governance_copilot::SEATS_REPORT_TYPE,
        day,
    )
    .unwrap();
    assert_eq!(rows.len(), 2);
    let n = upsert_seat_snapshot(&pool, &t, org, &rows).await.unwrap();
    upsert_manifest(
        &pool,
        &t,
        "github_copilot",
        org,
        governance_copilot::SEATS_REPORT_TYPE,
        day,
        "ok",
        n,
    )
    .await
    .unwrap();

    let drift = verify_manifests(&pool, &t, "github_copilot", org)
        .await
        .unwrap();
    assert!(
        drift.is_empty(),
        "a clean seat snapshot must verify clean, not error on an unrecognized report type: \
         {drift:?}"
    );

    cratestack::sqlx::query(
        "DELETE FROM copilot_seat_snapshots \
         WHERE tenant_id = $1 AND organization_id = $2 AND snapshot_day = CAST($3 AS date) \
           AND provider_user_id = '2'",
    )
    .bind(&t)
    .bind(org)
    .bind(day)
    .execute(&pool)
    .await
    .unwrap();

    let drift = verify_manifests(&pool, &t, "github_copilot", org)
        .await
        .unwrap();
    assert_eq!(
        drift.len(),
        1,
        "deleted seat row must surface as drift: {drift:?}"
    );
    assert_eq!(drift[0].report, governance_copilot::SEATS_REPORT_TYPE);
    assert_eq!(drift[0].expected, 2);
    assert_eq!(drift[0].actual, 1);
}
