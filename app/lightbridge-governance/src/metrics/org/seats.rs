use std::time::Duration;

use cratestack::sqlx::PgPool;

use super::super::Metrics;

/// Refreshes the `governance_org_*` seat gauges from
/// `copilot_seat_snapshots` (`governance_core::org_kpis::org_seat_kpis`),
/// bounded by `timeout`. Kept in its own module because it is one of the two
/// independent derivation queries behind [`super::Metrics::refresh_org_kpis`]
/// -- usage and seats are different tables with independent failure modes,
/// so each is refreshed (and frozen on failure) independently.
pub(super) async fn refresh_org_seat_kpis(
    metrics: &Metrics,
    pool: &PgPool,
    tenant_id: &str,
    timeout: Duration,
) {
    let outcome = tokio::time::timeout(
        timeout,
        governance_core::org_kpis::org_seat_kpis(pool, tenant_id),
    )
    .await;

    let rows = match outcome {
        Ok(Ok(rows)) => rows,
        Ok(Err(error)) => {
            tracing::warn!(error = %error, "governance_org_* seats refresh: query failed");
            metrics
                .connector_metrics_scrape_errors_total
                .with_label_values(&["org_seats_query_error"])
                .inc();
            return;
        }
        Err(_elapsed) => {
            tracing::warn!(
                timeout_ms = timeout.as_millis(),
                "governance_org_* seats refresh: timed out"
            );
            metrics
                .connector_metrics_scrape_errors_total
                .with_label_values(&["org_seats_timeout"])
                .inc();
            return;
        }
    };

    metrics
        .org_kpi_has_data
        .with_label_values(&["seats"])
        .set(i64::from(!rows.is_empty()));

    for row in &rows {
        metrics
            .org_seats_assigned
            .with_label_values(&[&row.organization_id])
            .set(row.seats_assigned);
        metrics
            .org_seats_never_used
            .with_label_values(&[&row.organization_id])
            .set(row.seats_never_used);
    }
}
