//! Tests for [`super`]. Split into its own file (issue #175) rather than
//! raising the already-grandfathered LoC ceiling -- the same move
//! `otel/tests.rs` made, applied to the whole `mod tests` block.

use cratestack::{cratestack_error_from_sqlx, sqlx};
use sqlx::PgPool;

use super::{org_seat_kpis, org_usage_kpis};

/// Runs against a real Postgres when `DATABASE_URL` is set, mirroring
/// `connector_metrics.rs`'s own gated integration tests. Reports (via
/// `eprintln!`) rather than vanishing silently when skipped.
async fn connected_pool() -> Option<PgPool> {
    let database_url = std::env::var("DATABASE_URL").ok()?;
    let pool = PgPool::connect(&database_url).await.expect("connect");
    static MIGRATION_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    {
        let _guard = MIGRATION_LOCK.lock().await;
        crate::migrate::run(&pool).await.expect("migrate");
    }
    Some(pool)
}

async fn insert_org_daily(
    pool: &PgPool,
    tenant_id: &str,
    org: &str,
    report_day: &str,
    active_users: i64,
    engaged_users: i64,
    net_cost_micro_usd: i64,
) {
    sqlx::query(
        "INSERT INTO copilot_org_dailys \
         (id, tenant_id, organization_id, report_day, active_users, engaged_users, \
          total_interactions, code_generations, code_acceptances, loc_suggested, \
          loc_added, loc_deleted, ai_credits, net_cost_micro_usd) \
         VALUES ($1, $2, $3, CAST($4 AS date), $5, $6, 0, 0, 0, 0, 0, 0, 0, $7)",
    )
    .bind(format!("org-kpi-test:{tenant_id}:{org}:{report_day}"))
    .bind(tenant_id)
    .bind(org)
    .bind(report_day)
    .bind(active_users)
    .bind(engaged_users)
    .bind(net_cost_micro_usd)
    .execute(pool)
    .await
    .map_err(cratestack_error_from_sqlx)
    .expect("insert org daily fixture");
}

async fn insert_seat(
    pool: &PgPool,
    tenant_id: &str,
    org: &str,
    snapshot_day: &str,
    provider_user_id: &str,
    last_activity_at: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO copilot_seat_snapshots \
         (id, tenant_id, organization_id, snapshot_day, provider_user_id, user_login, \
          seat_assigned_at, last_activity_at, last_activity_editor, seat_state) \
         VALUES ($1, $2, $3, CAST($4 AS date), $5, $5, now(), \
                 CAST($6 AS timestamptz), NULL, 'active')",
    )
    .bind(format!(
        "org-kpi-seat-test:{tenant_id}:{org}:{provider_user_id}"
    ))
    .bind(tenant_id)
    .bind(org)
    .bind(snapshot_day)
    .bind(provider_user_id)
    .bind(last_activity_at)
    .execute(pool)
    .await
    .map_err(cratestack_error_from_sqlx)
    .expect("insert seat fixture");
}

/// A tenant with zero `copilot_org_dailys` rows yields an empty `Vec`,
/// not a fabricated zero-valued row -- proves the query does not default
/// a missing organization into a "genuinely zero" reading.
#[tokio::test]
async fn a_tenant_with_no_usage_rows_yields_no_rows() {
    let Some(pool) = connected_pool().await else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let tenant_id = format!("tenant-org-kpi-empty-{}", cuid::cuid2());

    let rows = org_usage_kpis(&pool, &tenant_id)
        .await
        .expect("query succeeds");

    assert!(
        rows.is_empty(),
        "a tenant with zero copilot_org_dailys rows must report zero organizations, not a \
         fabricated one"
    );
}

/// Today has no row, but an earlier day does: the query must select the
/// most recent AVAILABLE day, not assume `CURRENT_DATE` -- otherwise the
/// gauges would read as absent (or, worse, as zero) for the entire part
/// of the day before that day's report is published, and any alert
/// wired to them fires spuriously.
#[tokio::test]
async fn selects_the_latest_available_day_not_today() {
    let Some(pool) = connected_pool().await else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let tenant_id = format!("tenant-org-kpi-latest-{}", cuid::cuid2());
    let org = "org-latest";
    let ten_days_ago = (chrono::Utc::now() - chrono::Duration::days(10))
        .format("%Y-%m-%d")
        .to_string();
    let three_days_ago = (chrono::Utc::now() - chrono::Duration::days(3))
        .format("%Y-%m-%d")
        .to_string();

    // Two rows, neither dated today, so MIN and MAX genuinely disagree --
    // this is what makes the assertion below actually exercise "latest",
    // not just "the only row that happens to exist".
    insert_org_daily(&pool, &tenant_id, org, &ten_days_ago, 7, 2, 1_000).await;
    insert_org_daily(&pool, &tenant_id, org, &three_days_ago, 42, 10, 1_000_000).await;
    // Deliberately no row for today.

    let rows = org_usage_kpis(&pool, &tenant_id)
        .await
        .expect("query succeeds");

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].report_day.format("%Y-%m-%d").to_string(),
        three_days_ago,
        "must report the latest AVAILABLE day (three_days_ago), not today (which has no \
         row) and not the older ten_days_ago row"
    );
    assert_eq!(rows[0].active_users, 42);
}

/// Month-to-date must sum only days within the latest available day's
/// calendar month -- a cost row from the previous month must not leak
/// into the current month's total.
#[tokio::test]
async fn month_to_date_excludes_the_previous_month() {
    let Some(pool) = connected_pool().await else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let tenant_id = format!("tenant-org-kpi-mtd-{}", cuid::cuid2());
    let org = "org-mtd";

    // A fixed "latest day" near the start of a month, so "the previous
    // month" and "this month" are unambiguous regardless of when this
    // test runs.
    insert_org_daily(&pool, &tenant_id, org, "2026-03-31", 1, 1, 5_000_000).await;
    insert_org_daily(&pool, &tenant_id, org, "2026-04-01", 1, 1, 2_000_000).await;
    insert_org_daily(&pool, &tenant_id, org, "2026-04-02", 1, 1, 3_000_000).await;

    let rows = org_usage_kpis(&pool, &tenant_id)
        .await
        .expect("query succeeds");

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].report_day.format("%Y-%m-%d").to_string(),
        "2026-04-02",
        "latest day must be 2026-04-02"
    );
    assert_eq!(
        rows[0].daily_cost_micro_usd, 3_000_000,
        "daily cost must be only the latest day's own cost"
    );
    assert_eq!(
        rows[0].cost_month_to_date_micro_usd, 5_000_000,
        "month-to-date must sum 04-01 + 04-02 (2_000_000 + 3_000_000) and exclude \
         03-31, which is the previous month -- got a total that suggests 03-31 leaked in"
    );
}

/// A tenant's rows must not leak into another tenant's result --
/// `tenant_id` is in the WHERE clause of every CTE and join, not
/// decoration (ADR-0001).
#[tokio::test]
async fn a_tenants_rows_do_not_leak_into_another_tenants_result() {
    let Some(pool) = connected_pool().await else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let tenant_a = format!("tenant-org-kpi-a-{}", cuid::cuid2());
    let tenant_b = format!("tenant-org-kpi-b-{}", cuid::cuid2());
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    // Deliberately the SAME organization_id and report_day for both
    // tenants -- if `tenant_id` were ever dropped from the query (CTE or
    // final join), the join's `organization_id`/`report_day` match alone
    // would still line the two tenants' rows up and either double the
    // result or substitute tenant_b's numbers for tenant_a's. Two
    // different org names would not catch that: the join would fail to
    // match across tenants regardless of a missing tenant filter, and
    // the bug would go undetected.
    insert_org_daily(&pool, &tenant_a, "org-shared", &today, 5, 3, 1_000_000).await;
    insert_org_daily(
        &pool,
        &tenant_b,
        "org-shared",
        &today,
        999,
        999,
        999_000_000,
    )
    .await;

    let rows = org_usage_kpis(&pool, &tenant_a)
        .await
        .expect("query succeeds");

    assert_eq!(
        rows.len(),
        1,
        "must see exactly tenant_a's one organization row, not tenant_b's too"
    );
    assert_eq!(rows[0].organization_id, "org-shared");
    assert_eq!(
        rows[0].active_users, 5,
        "must be tenant_a's own active_users (5), not tenant_b's (999)"
    );
}

/// A tenant with zero `copilot_seat_snapshots` rows yields an empty
/// `Vec` -- mirrors the usage-side "no data at all" test.
#[tokio::test]
async fn a_tenant_with_no_seat_rows_yields_no_rows() {
    let Some(pool) = connected_pool().await else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let tenant_id = format!("tenant-org-kpi-seats-empty-{}", cuid::cuid2());

    let rows = org_seat_kpis(&pool, &tenant_id)
        .await
        .expect("query succeeds");

    assert!(
        rows.is_empty(),
        "a tenant with zero copilot_seat_snapshots rows must report zero organizations"
    );
}

/// `last_activity_at IS NULL` must be counted as never-used, and a seat
/// with a real `last_activity_at` must not be -- the licence-waste
/// signal this gauge exists for.
#[tokio::test]
async fn never_used_seats_are_counted_by_null_last_activity() {
    let Some(pool) = connected_pool().await else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let tenant_id = format!("tenant-org-kpi-seats-{}", cuid::cuid2());
    let org = "org-seats";
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    insert_seat(
        &pool,
        &tenant_id,
        org,
        &today,
        "user-used",
        Some("2026-08-01T00:00:00Z"),
    )
    .await;
    insert_seat(&pool, &tenant_id, org, &today, "user-never-used", None).await;
    insert_seat(&pool, &tenant_id, org, &today, "user-also-never-used", None).await;

    let rows = org_seat_kpis(&pool, &tenant_id)
        .await
        .expect("query succeeds");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].seats_assigned, 3, "all three seats are assigned");
    assert_eq!(
        rows[0].seats_never_used, 2,
        "exactly the two seats with last_activity_at IS NULL must count as never-used"
    );
}
