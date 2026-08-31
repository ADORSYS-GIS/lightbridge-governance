//! Org-level KPI queries derived from `copilot_org_dailys` and
//! `copilot_seat_snapshots`, for the always-on API's `/metrics` scrape.
//!
//! This is a deliberate, bounded exception to ADR-0003's "Mimir keeps only
//! the ~10 low-cardinality `governance_connector_*` operational metrics".
//! ADR-0003's cardinality argument is unchanged and still governs: business
//! *detail* -- per-user, per-repo, per-team numbers -- stays in SQL/Grafana,
//! because those dimensions are a cardinality bomb. But a handful of
//! **org-level aggregates** carry no unbounded dimension (at most an
//! `organization_id`, and a deployment has few organizations -- ADR-0001 is
//! single-tenant per deployment, and `governance-ctl`'s `Config` syncs
//! exactly one GitHub org per run), and alerting needs them in Mimir because
//! nobody can page off a Grafana SQL panel. "Monthly spend exceeded X" and
//! "active users dropped 30%" are alert questions, not dashboard questions.
//!
//! Same shape as `connector_metrics.rs` (ADR-0007): this module owns the
//! queries only. It does not decide how a result becomes a Prometheus
//! series, does not impose a timeout, and does not decide what a query
//! failure looks like on `/metrics` -- that is
//! `app/lightbridge-governance/src/metrics.rs`'s job, because "unavailable
//! must never look healthy" belongs at the scrape boundary.
//!
//! Deliberately NOT pushed through the copilot-sync OTel collector
//! (ADR-0011): that collector's state is in-memory only
//! (`replicas: 1`, no PodDisruptionBudget), so a restart blanks every series
//! until the next CronJob run, up to 6h later -- ADR-0011 explicitly classes
//! those metrics as dashboard-grade, not alert-grade, for exactly that
//! reason. Re-deriving from Postgres on every scrape, as this module does,
//! has none of that: the value is a fact about the database, not a cached
//! push, so it survives an API restart the same way `connector_freshness`
//! does.
//!
//! ## "Latest available day", per organization
//!
//! Copilot data lags by design (RFC-0001's 3-day lookback) and a day can be
//! missing entirely, so every query here selects on `MAX(report_day)` /
//! `MAX(snapshot_day)` rather than assuming `CURRENT_DATE` -- otherwise every
//! gauge reads zero for part of each day and any alert on them fires
//! spuriously. That `MAX` is taken **per `organization_id`**, not once across
//! the whole tenant: if a tenant ever has more than one organization's data
//! and one lags behind the other, a single tenant-wide `MAX` would either
//! misattribute a stale organization's numbers to a day it does not have
//! data for, or silently drop it from the result. Grouping by
//! `organization_id` first means each organization's numbers are always
//! reported against its own most recent day.
//!
//! ## Money stays integer micro-USD (ADR-0008)
//!
//! `net_cost_micro_usd` is `BIGINT` end to end here, including the
//! month-to-date `SUM` (cast back to `BIGINT` -- Postgres widens
//! `SUM(bigint)` to `NUMERIC` to avoid silent overflow, and casting back
//! keeps the Rust side an `i64`, never a decimal/float type). The boundary
//! worth stating explicitly, not leaving implicit: a Prometheus gauge's wire
//! value is `float64`, whose exact-integer range is `2^53` (~9.007e15)
//! micro-USD, i.e. ~$9 billion. This deployment's spend is nowhere close to
//! that, so the integer contract survives the `/metrics` text exposition and
//! Prometheus's own storage, but the ceiling is real and worth knowing
//! rather than assuming "int in, therefore fine forever".
//!
//! ## Absent vs. zero
//!
//! A tenant with zero `copilot_org_dailys`/`copilot_seat_snapshots` rows at
//! all yields an **empty `Vec`** here, exactly like `connector_freshness`
//! does for a never-synced provider -- callers must not fold "no rows" into
//! a fabricated zero-valued reading. This is the same trap
//! `governance_connector_has_synced` exists to avoid, so the metrics layer
//! pairs each family with its own unlabeled `..._has_data` gauge (`1` once a
//! query has confirmed at least one row exists for the tenant, `0` once a
//! query has confirmed there are none, absent until the first successful
//! query) -- see `app/lightbridge-governance/src/metrics.rs`.

use chrono::{DateTime, Utc};
use cratestack::{cratestack_error_from_sqlx, sqlx};
use sqlx::PgPool;

use crate::{Error, Result};

/// One organization's usage KPIs as of its own most recent available report
/// day. `report_day` is exposed so callers can tell which day the numbers
/// belong to (useful for logging/debugging a surprising reading), even
/// though it is not itself turned into a metric.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgUsageKpis {
    pub organization_id: String,
    pub report_day: DateTime<Utc>,
    /// Active users on `report_day` (Copilot's own "used at least one
    /// completion or chat" definition).
    pub active_users: i64,
    /// Engaged users on `report_day` (Copilot's own, narrower-than-active
    /// definition).
    pub engaged_users: i64,
    /// Integer micro-USD (ADR-0008): net cost recorded for `report_day` alone.
    pub daily_cost_micro_usd: i64,
    /// Integer micro-USD (ADR-0008): net cost summed from the first day of
    /// `report_day`'s calendar month through `report_day` itself. Only the
    /// days actually ingested are summed -- a gap inside the month is not
    /// backfilled with zero or otherwise estimated.
    pub cost_month_to_date_micro_usd: i64,
}

/// One organization's seat KPIs as of its own most recent seat snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgSeatKpis {
    pub organization_id: String,
    pub snapshot_day: DateTime<Utc>,
    /// Total seats assigned as of `snapshot_day`.
    pub seats_assigned: i64,
    /// Seats assigned as of `snapshot_day` with `last_activity_at IS NULL`
    /// -- assigned but never used at all. The licence-waste signal.
    pub seats_never_used: i64,
}

/// Org-level usage KPIs (active/engaged users, daily and month-to-date
/// cost), one row per organization that has ever recorded a
/// `copilot_org_dailys` row for `tenant_id`. An organization with zero rows
/// is simply absent from the returned `Vec` -- see the module doc comment on
/// why callers must treat "not present" as its own state, not a default.
///
/// # Errors
///
/// Returns [`Error::Storage`] if the query fails. Imposes no timeout of its
/// own -- see `connector_freshness`'s doc comment for why that is the
/// scrape boundary's job, not this query helper's.
pub async fn org_usage_kpis(pool: &PgPool, tenant_id: &str) -> Result<Vec<OrgUsageKpis>> {
    let rows: Vec<(String, DateTime<Utc>, i64, i64, i64, i64)> = sqlx::query_as(
        "WITH latest AS ( \
           SELECT organization_id, MAX(report_day) AS max_day \
           FROM copilot_org_dailys \
           WHERE tenant_id = $1 \
           GROUP BY organization_id \
         ), \
         month_to_date AS ( \
           SELECT o.organization_id, \
                  CAST(SUM(o.net_cost_micro_usd) AS BIGINT) AS cost_month_to_date_micro_usd \
           FROM copilot_org_dailys o \
           JOIN latest l ON l.organization_id = o.organization_id \
           WHERE o.tenant_id = $1 \
             AND o.report_day >= date_trunc('month', l.max_day) \
             AND o.report_day <= l.max_day \
           GROUP BY o.organization_id \
         ) \
         SELECT o.organization_id, o.report_day, o.active_users, o.engaged_users, \
                o.net_cost_micro_usd, m.cost_month_to_date_micro_usd \
         FROM copilot_org_dailys o \
         JOIN latest l ON l.organization_id = o.organization_id AND l.max_day = o.report_day \
         JOIN month_to_date m ON m.organization_id = o.organization_id \
         WHERE o.tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_all(pool)
    .await
    .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;

    Ok(rows
        .into_iter()
        .map(
            |(
                organization_id,
                report_day,
                active_users,
                engaged_users,
                daily_cost_micro_usd,
                cost_month_to_date_micro_usd,
            )| OrgUsageKpis {
                organization_id,
                report_day,
                active_users,
                engaged_users,
                daily_cost_micro_usd,
                cost_month_to_date_micro_usd,
            },
        )
        .collect())
}

/// Org-level seat KPIs (assigned, never-used), one row per organization that
/// has ever recorded a `copilot_seat_snapshots` row for `tenant_id`. Same
/// absent-vs-zero contract as [`org_usage_kpis`].
///
/// # Errors
///
/// Returns [`Error::Storage`] if the query fails.
pub async fn org_seat_kpis(pool: &PgPool, tenant_id: &str) -> Result<Vec<OrgSeatKpis>> {
    let rows: Vec<(String, DateTime<Utc>, i64, i64)> = sqlx::query_as(
        "WITH latest AS ( \
           SELECT organization_id, MAX(snapshot_day) AS max_day \
           FROM copilot_seat_snapshots \
           WHERE tenant_id = $1 \
           GROUP BY organization_id \
         ) \
         SELECT s.organization_id, s.snapshot_day, \
                COUNT(*) AS seats_assigned, \
                COUNT(*) FILTER (WHERE s.last_activity_at IS NULL) AS seats_never_used \
         FROM copilot_seat_snapshots s \
         JOIN latest l ON l.organization_id = s.organization_id AND l.max_day = s.snapshot_day \
         WHERE s.tenant_id = $1 \
         GROUP BY s.organization_id, s.snapshot_day",
    )
    .bind(tenant_id)
    .fetch_all(pool)
    .await
    .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;

    Ok(rows
        .into_iter()
        .map(
            |(organization_id, snapshot_day, seats_assigned, seats_never_used)| OrgSeatKpis {
                organization_id,
                snapshot_day,
                seats_assigned,
                seats_never_used,
            },
        )
        .collect())
}

#[cfg(test)]
mod tests {
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
}
