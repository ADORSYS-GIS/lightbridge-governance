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
mod tests;
