use std::time::{Duration, Instant};

use super::{super::Metrics, connected_pool, unreachable_pool};

#[test]
fn a_fresh_registry_exposes_no_org_kpi_reading_at_all() {
    // Same contract as connector freshness: before any refresh has ever
    // run, the org KPI gauges and has_data flags must be completely
    // absent, not fabricated zeros.
    let out = Metrics::new().render();
    for series in [
        "governance_org_active_users",
        "governance_org_engaged_users",
        "governance_org_daily_cost_micro_usd",
        "governance_org_cost_month_to_date_micro_usd",
        "governance_org_seats_assigned",
        "governance_org_seats_never_used",
        "governance_org_kpi_has_data",
    ] {
        assert!(
            !out.contains(series),
            "{series} must not render before any refresh has run -- got:\n{out}"
        );
    }
    // The scrape-error counter's new org_* reasons, by contrast, are a
    // legitimate 0 at process start -- present so increase() has a
    // series to watch from the first scrape.
    for reason in [
        "org_usage_timeout",
        "org_usage_query_error",
        "org_seats_timeout",
        "org_seats_query_error",
    ] {
        assert!(
            out.contains(&format!(
                "governance_connector_metrics_scrape_errors_total{{reason=\"{reason}\"}} 0"
            )),
            "reason={reason} must be present at 0 from process start -- got:\n{out}"
        );
    }
}

#[tokio::test]
async fn a_db_outage_never_produces_a_healthy_looking_org_kpi_reading() {
    let metrics = Metrics::new();
    let pool = unreachable_pool();

    let start = Instant::now();
    metrics
        .refresh_org_kpis(&pool, "tenant-org-kpi-outage", Duration::from_millis(200))
        .await;
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "must fail within the configured timeout (usage + seats sequentially, so up to \
         ~2x), not sqlx's 30s pool default -- took {elapsed:?}"
    );

    let out = metrics.render();
    for series in [
        "governance_org_active_users",
        "governance_org_engaged_users",
        "governance_org_daily_cost_micro_usd",
        "governance_org_cost_month_to_date_micro_usd",
        "governance_org_seats_assigned",
        "governance_org_seats_never_used",
        "governance_org_kpi_has_data",
    ] {
        assert!(
            !out.contains(series),
            "an outage must not fabricate {series} -- got:\n{out}"
        );
    }
    assert!(
        out.contains(
            "governance_connector_metrics_scrape_errors_total{reason=\"org_usage_timeout\"} 1"
        ),
        "the usage-side outage must be visible via the error counter -- got:\n{out}"
    );
    assert!(
        out.contains(
            "governance_connector_metrics_scrape_errors_total{reason=\"org_seats_timeout\"} 1"
        ),
        "the seats-side outage must be visible via the error counter -- got:\n{out}"
    );
}

#[tokio::test]
async fn a_failed_org_kpi_refresh_leaves_a_previously_good_reading_in_place() {
    // Mirrors `a_failed_refresh_leaves_a_previously_good_reading_in_place_rather_than_erasing_it`
    // for the org KPI family: once a value is known good, a later failed
    // refresh must not erase it back to "unknown" -- see
    // `Metrics::refresh_org_kpis`'s doc comment on why freezing (not
    // zeroing) is the safe choice here specifically to avoid an outage
    // reading as "active users dropped to zero".
    let metrics = Metrics::new();
    metrics
        .org_active_users
        .with_label_values(&["org-known-good"])
        .set(123);
    metrics
        .org_kpi_has_data
        .with_label_values(&["usage"])
        .set(1);
    metrics
        .org_seats_never_used
        .with_label_values(&["org-known-good"])
        .set(4);
    metrics
        .org_kpi_has_data
        .with_label_values(&["seats"])
        .set(1);

    let pool = unreachable_pool();
    metrics
        .refresh_org_kpis(&pool, "tenant-org-kpi-outage-2", Duration::from_millis(200))
        .await;

    let out = metrics.render();
    assert!(
        out.contains("governance_org_active_users{organization_id=\"org-known-good\"} 123"),
        "a failed refresh must not erase a previously observed active_users reading -- \
         got:\n{out}"
    );
    assert!(
        out.contains("governance_org_kpi_has_data{family=\"usage\"} 1"),
        "a failed refresh must not erase a previously observed has_data reading -- \
         got:\n{out}"
    );
    assert!(
        out.contains("governance_org_seats_never_used{organization_id=\"org-known-good\"} 4"),
        "a failed refresh must not erase a previously observed seats_never_used reading -- \
         got:\n{out}"
    );
    assert!(out.contains("governance_org_kpi_has_data{family=\"seats\"} 1"));
}

/// End-to-end against a real database: a tenant with zero
/// `copilot_org_dailys`/`copilot_seat_snapshots` rows must render
/// `..._has_data 0` for both families and no per-organization gauge at
/// all -- proving "no data at all" is visibly distinct from "genuinely
/// zero" (which would render the gauges present, at `0`).
#[tokio::test]
async fn a_tenant_with_no_org_kpi_data_renders_has_data_zero_not_a_healthy_looking_gap() {
    let Some(pool) = connected_pool().await else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let metrics = Metrics::new();
    let tenant_id = format!("tenant-org-kpi-no-data-{}", cuid::cuid2());

    metrics
        .refresh_org_kpis(&pool, &tenant_id, Duration::from_secs(3))
        .await;

    let out = metrics.render();
    assert!(
        out.contains("governance_org_kpi_has_data{family=\"usage\"} 0"),
        "a tenant with zero copilot_org_dailys rows must report has_data{{family=usage}}=0 \
         -- got:\n{out}"
    );
    assert!(
        out.contains("governance_org_kpi_has_data{family=\"seats\"} 0"),
        "a tenant with zero copilot_seat_snapshots rows must report \
         has_data{{family=seats}}=0 -- got:\n{out}"
    );
    assert!(
        !out.contains("governance_org_active_users"),
        "must not render a fabricated per-organization gauge for a tenant with no data -- \
         got:\n{out}"
    );
    assert!(
        out.contains(
            "governance_connector_metrics_scrape_errors_total{reason=\"org_usage_timeout\"} 0"
        ),
        "a successful refresh against a real, reachable DB must not count as an error"
    );
}
