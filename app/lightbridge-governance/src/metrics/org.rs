use std::time::Duration;

use cratestack::sqlx::PgPool;
use prometheus::{IntGaugeVec, Registry, opts};

use super::Metrics;

mod seats;

/// The `governance_org_*` gauges, built and registered by [`build`] and then
/// moved into [`Metrics`]. Fields are `pub(super)` so the parent module can
/// assemble the struct; none is part of the public API.
pub(super) struct OrgGauges {
    pub(super) active_users: IntGaugeVec,
    pub(super) engaged_users: IntGaugeVec,
    pub(super) daily_cost_micro_usd: IntGaugeVec,
    pub(super) cost_month_to_date_micro_usd: IntGaugeVec,
    pub(super) seats_assigned: IntGaugeVec,
    pub(super) seats_never_used: IntGaugeVec,
    pub(super) has_data: IntGaugeVec,
}

/// Constructs and registers the `governance_org_*` family.
#[expect(
    clippy::expect_used,
    reason = "impossibility proof: metric construction only fails on duplicate names or \
              invalid help text, and both are compile-time string literals here"
)]
pub(super) fn build(registry: &Registry) -> OrgGauges {
    let active_users = IntGaugeVec::new(
        opts!(
            "governance_org_active_users",
            "active users on the organization's most recent AVAILABLE report day; absent \
             until a refresh has observed a row for that organization"
        ),
        &["organization_id"],
    )
    .expect("static metric definition");
    let engaged_users = IntGaugeVec::new(
        opts!(
            "governance_org_engaged_users",
            "engaged users on the organization's most recent AVAILABLE report day; same \
             absence contract as governance_org_active_users"
        ),
        &["organization_id"],
    )
    .expect("static metric definition");
    let daily_cost_micro_usd = IntGaugeVec::new(
        opts!(
            "governance_org_daily_cost_micro_usd",
            "net Copilot cost, integer micro-USD (ADR-0008), on the organization's most \
             recent AVAILABLE report day -- estimated, not reconciled invoiced spend"
        ),
        &["organization_id"],
    )
    .expect("static metric definition");
    let cost_month_to_date_micro_usd = IntGaugeVec::new(
        opts!(
            "governance_org_cost_month_to_date_micro_usd",
            "net Copilot cost, integer micro-USD (ADR-0008), summed from the first day of \
             the most recent available report day's calendar month through that day"
        ),
        &["organization_id"],
    )
    .expect("static metric definition");
    let seats_assigned = IntGaugeVec::new(
        opts!(
            "governance_org_seats_assigned",
            "seats assigned as of the organization's most recent seat snapshot"
        ),
        &["organization_id"],
    )
    .expect("static metric definition");
    let seats_never_used = IntGaugeVec::new(
        opts!(
            "governance_org_seats_never_used",
            "seats assigned as of the most recent snapshot with last_activity_at IS NULL -- \
             the licence-waste signal"
        ),
        &["organization_id"],
    )
    .expect("static metric definition");
    let has_data = IntGaugeVec::new(
        opts!(
            "governance_org_kpi_has_data",
            "1 once a refresh has confirmed at least one row exists for this tenant in the \
             family table (family=usage -> copilot_org_dailys, family=seats -> \
             copilot_seat_snapshots), 0 once confirmed there are none, absent before that \
             family's first successful refresh"
        ),
        &["family"],
    )
    .expect("static metric definition");
    super::register(
        registry,
        vec![
            Box::new(active_users.clone()),
            Box::new(engaged_users.clone()),
            Box::new(daily_cost_micro_usd.clone()),
            Box::new(cost_month_to_date_micro_usd.clone()),
            Box::new(seats_assigned.clone()),
            Box::new(seats_never_used.clone()),
            Box::new(has_data.clone()),
        ],
    );
    OrgGauges {
        active_users,
        engaged_users,
        daily_cost_micro_usd,
        cost_month_to_date_micro_usd,
        seats_assigned,
        seats_never_used,
        has_data,
    }
}

impl Metrics {
    /// Refreshes `governance_org_*` from `copilot_org_dailys` and
    /// `copilot_seat_snapshots` (`governance_core::org_kpis`). Same
    /// refresh-on-scrape shape and timeout discipline as
    /// [`Self::refresh_connector_freshness`], run as two independent bounded
    /// queries rather than one: usage and seats are different tables with
    /// independent failure modes (e.g. a lock contended on one table but not
    /// the other), and keeping them independent means a seat-snapshot query
    /// failure does not also blank out usage gauges that queried
    /// successfully, and vice versa -- each family's gauges freeze at their
    /// last known value exactly as `refresh_connector_freshness` already
    /// does, for the identical reason: a value observed from a completed
    /// query is a fact about that query's moment in time, and does not
    /// become false just because a later refresh could not confirm it again.
    /// Deliberately NOT touching the gauges (rather than zeroing them) on
    /// failure is what keeps a Postgres outage from reading as "active users
    /// dropped to zero" -- see the module doc comment and
    /// `docs/adr/0003-grafana-reads-postgres-directly.md`.
    pub async fn refresh_org_kpis(&self, pool: &PgPool, tenant_id: &str, timeout: Duration) {
        self.refresh_org_usage_kpis(pool, tenant_id, timeout).await;
        seats::refresh_org_seat_kpis(self, pool, tenant_id, timeout).await;
    }

    async fn refresh_org_usage_kpis(&self, pool: &PgPool, tenant_id: &str, timeout: Duration) {
        let outcome = tokio::time::timeout(
            timeout,
            governance_core::org_kpis::org_usage_kpis(pool, tenant_id),
        )
        .await;

        let rows = match outcome {
            Ok(Ok(rows)) => rows,
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "governance_org_* usage refresh: query failed");
                self.connector_metrics_scrape_errors_total
                    .with_label_values(&["org_usage_query_error"])
                    .inc();
                return;
            }
            Err(_elapsed) => {
                tracing::warn!(
                    timeout_ms = timeout.as_millis(),
                    "governance_org_* usage refresh: timed out"
                );
                self.connector_metrics_scrape_errors_total
                    .with_label_values(&["org_usage_timeout"])
                    .inc();
                return;
            }
        };

        // A successful query is what lets `family="usage"` move away from
        // "absent" -- an empty Vec is a CONFIRMED "no data", not an unknown,
        // so `0` (not left absent) is correct here. See the module doc
        // comment / `org_kpis`'s absent-vs-zero contract.
        self.org_kpi_has_data
            .with_label_values(&["usage"])
            .set(i64::from(!rows.is_empty()));

        for row in &rows {
            self.org_active_users
                .with_label_values(&[&row.organization_id])
                .set(row.active_users);
            self.org_engaged_users
                .with_label_values(&[&row.organization_id])
                .set(row.engaged_users);
            self.org_daily_cost_micro_usd
                .with_label_values(&[&row.organization_id])
                .set(row.daily_cost_micro_usd);
            self.org_cost_month_to_date_micro_usd
                .with_label_values(&[&row.organization_id])
                .set(row.cost_month_to_date_micro_usd);
        }
    }
}
