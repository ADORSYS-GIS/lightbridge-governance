//! Connector operational freshness derived from `ingest_manifests` (ADR-0007).
//!
//! A CronJob pod cannot be scraped, so the collector records every run
//! outcome in `ingest_manifests` and the always-running API derives
//! `governance_connector_*` from that table instead. This module owns the
//! one query that state requires. It deliberately does not decide how the
//! result becomes Prometheus series, does not impose a timeout, and does not
//! decide what a query failure should look like on `/metrics` -- all of that
//! is `app/lightbridge-governance/src/metrics.rs`'s job, because the
//! "unavailable must never look healthy" decision belongs at the scrape
//! boundary, not buried in a query helper.

use chrono::{DateTime, Utc};
use cratestack::{cool_error_from_sqlx, sqlx};
use sqlx::PgPool;

use crate::{Error, Result};

/// The most recent report day `provider` has successfully ingested at least
/// one report for.
///
/// `last_success_at` is `report_day` (the collector stores it as a date cast
/// to `timestamptz`, i.e. midnight UTC of that calendar day), not
/// `completed_at`: the alerts this backs (`docs/runbooks/copilot-sync-failed.md`'s
/// "no successful sync in 36h" / "report older than 72h") are about the
/// freshness of the *data*, matching `governance-ctl status`'s existing
/// `high_water_mark`-based definition of "last success" -- not about when our
/// own process last happened to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorFreshness {
    pub provider: String,
    pub last_success_at: DateTime<Utc>,
}

/// One row per provider that has EVER recorded a successful manifest for
/// `tenant_id`. A provider with zero manifest rows is simply absent from the
/// returned `Vec` -- callers must treat "not present" as its own state
/// (RFC-0001's go-live bullet: a deployment that has never synced must not be
/// indistinguishable from a healthy one), not fold it into a default value.
///
/// `status IN ('ok', 'empty')` matches exactly the two outcomes
/// `governance_copilot`'s sync path ever writes on a completed run for a
/// report -- a fetch/parse/store failure returns before `upsert_manifest` is
/// ever called, so there is today no third status that would need excluding.
/// The filter stays explicit anyway (not "any row"): if a future status is
/// added for a completed-but-degraded run, it does not silently start
/// counting as success just because a row exists.
///
/// `report_type <> 'billing-seats'` is load-bearing, not tidiness. Seat
/// snapshots are a *current-state* listing with no `day` parameter, so every
/// run writes a manifest stamped with TODAY. Counting those here would make
/// this gauge read fresh forever the moment seats succeeds -- even with all
/// four daily reports failing for a week -- silently disabling the
/// "no successful sync in 36h" alert this gauge exists to drive. Freshness
/// here means "how current is the ingested report DATA", and only the
/// day-based reports can answer that.
///
/// The literal is `governance_copilot::SEATS_REPORT_TYPE`, duplicated rather
/// than imported because `governance-copilot` depends on this crate, not the
/// other way round. `governance_copilot`'s own `high_water_mark` excludes the
/// same value for the same reason -- keep the two in step.
///
/// # Errors
///
/// Returns [`Error::Storage`] if the query fails. This function imposes no
/// timeout of its own -- the `/metrics` scrape path bounds it, and a caller
/// must not treat `Err` as "no connectors configured" (that would turn a
/// database outage into the exact silently-healthy reading ADR-0007 exists to
/// prevent).
pub async fn connector_freshness(
    pool: &PgPool,
    tenant_id: &str,
) -> Result<Vec<ConnectorFreshness>> {
    let rows: Vec<(String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT provider, MAX(report_day) FROM ingest_manifests \
         WHERE tenant_id = $1 AND status IN ('ok', 'empty') \
           AND report_type <> 'billing-seats' \
         GROUP BY provider",
    )
    .bind(tenant_id)
    .fetch_all(pool)
    .await
    .map_err(|e| Error::Storage(cool_error_from_sqlx(e)))?;

    Ok(rows
        .into_iter()
        .map(|(provider, last_success_at)| ConnectorFreshness {
            provider,
            last_success_at,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use cratestack::{cool_error_from_sqlx, sqlx};
    use sqlx::PgPool;

    use super::connector_freshness;

    /// Runs against a real Postgres when `DATABASE_URL` is set, mirroring
    /// `ingest.rs`'s gated integration test. Reports (via `eprintln!`,
    /// visible with `cargo test -- --nocapture`) rather than silently
    /// vanishing when skipped, so a CI run that forgets to set the env var is
    /// at least noisy, not just quietly green.
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

    async fn insert_manifest(
        pool: &PgPool,
        tenant_id: &str,
        provider: &str,
        report_day: &str,
        status: &str,
    ) {
        sqlx::query(
            "INSERT INTO ingest_manifests \
             (id, tenant_id, provider, scope_id, report_day, report_type, status, \
              record_count, schema_version, started_at, completed_at) \
             VALUES ($1, $2, $3, 'scope', CAST($4 AS date), 'organization-1-day', $5, 1, 1, \
                     now(), now())",
        )
        .bind(format!(
            "manifest-{tenant_id}-{provider}-{report_day}-{status}"
        ))
        .bind(tenant_id)
        .bind(provider)
        .bind(report_day)
        .bind(status)
        .execute(pool)
        .await
        .map_err(cool_error_from_sqlx)
        .expect("insert manifest fixture");
    }

    /// Inserts a manifest with an explicit `report_type`, for the seat-snapshot
    /// exclusion test below.
    async fn insert_manifest_of_type(
        pool: &PgPool,
        tenant_id: &str,
        provider: &str,
        report_day: &str,
        report_type: &str,
    ) {
        sqlx::query(
            "INSERT INTO ingest_manifests \
             (id, tenant_id, provider, scope_id, report_day, report_type, status, \
              record_count, schema_version, started_at, completed_at) \
             VALUES ($1, $2, $3, 'scope', CAST($4 AS date), $5, 'ok', 1, 1, now(), now())",
        )
        .bind(format!(
            "manifest-{tenant_id}-{provider}-{report_day}-{report_type}"
        ))
        .bind(tenant_id)
        .bind(provider)
        .bind(report_day)
        .bind(report_type)
        .execute(pool)
        .await
        .map_err(cool_error_from_sqlx)
        .expect("insert typed manifest fixture");
    }

    /// A seat snapshot must NOT count as report freshness.
    ///
    /// Seats are a current-state listing with no `day` parameter, so every run
    /// stamps a manifest with today. If this query counted them, the gauge
    /// would read fresh forever the moment seats succeeded -- even with every
    /// daily report failing -- silently disabling the "no successful sync in
    /// 36h" alert it exists to drive.
    ///
    /// The fixture is the exact shape that breaks it: daily reports stale by a
    /// week, a seat snapshot from today.
    #[tokio::test]
    async fn a_seat_snapshot_does_not_mask_stale_daily_reports() {
        let Some(pool) = connected_pool().await else {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        };
        let tenant_id = format!("tenant-seats-mask-{}", cuid::cuid2());
        let stale_day = (chrono::Utc::now() - chrono::Duration::days(7))
            .format("%Y-%m-%d")
            .to_string();
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

        insert_manifest_of_type(
            &pool,
            &tenant_id,
            "github_copilot",
            &stale_day,
            "organization-1-day",
        )
        .await;
        insert_manifest_of_type(&pool, &tenant_id, "github_copilot", &today, "billing-seats").await;

        let rows = connector_freshness(&pool, &tenant_id)
            .await
            .expect("query succeeds");

        let reported = rows
            .iter()
            .find(|r| r.provider == "github_copilot")
            .expect("provider present");
        assert_eq!(
            reported.last_success_at.format("%Y-%m-%d").to_string(),
            stale_day,
            "freshness must reflect the stale daily report, not today's seat snapshot -- \
             counting seats here makes the gauge permanently fresh and kills the alert"
        );
    }

    /// A tenant with zero `ingest_manifests` rows -- the "never synced" case
    /// -- must yield an empty result, not a row claiming success. Proves the
    /// query does not default a missing provider into a fabricated freshness
    /// value.
    #[tokio::test]
    async fn a_tenant_with_no_manifests_yields_no_rows() {
        let Some(pool) = connected_pool().await else {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        };
        let tenant_id = format!("tenant-never-synced-{}", cuid::cuid2());

        let rows = connector_freshness(&pool, &tenant_id)
            .await
            .expect("query succeeds");

        assert!(
            rows.is_empty(),
            "a tenant with zero manifest rows must report zero providers, not a fabricated one"
        );
    }

    /// The freshness query reports the most recent `ok`/`empty` day, and does
    /// not fold in another tenant's rows -- `tenant_id` is in the WHERE
    /// clause, not just decoration.
    #[tokio::test]
    async fn the_most_recent_successful_day_is_reported_per_provider() {
        let Some(pool) = connected_pool().await else {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        };
        let tenant_id = format!("tenant-fresh-{}", cuid::cuid2());
        let other_tenant = format!("tenant-other-{}", cuid::cuid2());

        insert_manifest(&pool, &tenant_id, "github_copilot", "2026-08-01", "ok").await;
        insert_manifest(&pool, &tenant_id, "github_copilot", "2026-08-03", "ok").await;
        insert_manifest(&pool, &other_tenant, "github_copilot", "2026-08-06", "ok").await;

        let rows = connector_freshness(&pool, &tenant_id)
            .await
            .expect("query succeeds");

        assert_eq!(rows.len(), 1, "exactly one provider for this tenant");
        assert_eq!(rows[0].provider, "github_copilot");
        assert_eq!(
            rows[0].last_success_at.date_naive().to_string(),
            "2026-08-03",
            "must report the MAX day, not the first or an unrelated tenant's later day"
        );
    }

    /// An `empty` manifest (GitHub's 204 -- no data published yet, not a
    /// failure) must still count as a successful run day, matching the
    /// runbook's own framing ("Job succeeded but data is stale -> GitHub has
    /// not published the report yet. ... this self-heals").
    #[tokio::test]
    async fn an_empty_report_day_counts_as_a_successful_run() {
        let Some(pool) = connected_pool().await else {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        };
        let tenant_id = format!("tenant-empty-{}", cuid::cuid2());
        insert_manifest(&pool, &tenant_id, "github_copilot", "2026-08-05", "empty").await;

        let rows = connector_freshness(&pool, &tenant_id)
            .await
            .expect("query succeeds");

        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].last_success_at.date_naive().to_string(),
            "2026-08-05"
        );
    }
}
