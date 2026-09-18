use std::time::{Duration, Instant};

use super::{super::Metrics, connected_pool, unreachable_pool};

#[test]
fn a_fresh_registry_exposes_no_connector_freshness_reading_at_all() {
    // Before any refresh has ever run (e.g. right after process start,
    // before the first /metrics scrape), the freshness family must be
    // completely absent -- not a 0 for either gauge, which would read as
    // "just synced" / "never synced" despite genuinely being unknown.
    let out = Metrics::new().render();
    assert!(
        !out.contains("governance_connector_last_success_timestamp_seconds"),
        "must not render a fabricated timestamp before any refresh has run"
    );
    assert!(
        !out.contains("governance_connector_has_synced"),
        "must not render a fabricated has_synced before any refresh has run"
    );
    // The error counter, by contrast, is a legitimate 0 at this point --
    // it must already be present so `increase()` has a series to watch.
    assert!(out.contains("governance_connector_metrics_scrape_errors_total{reason=\"timeout\"} 0"));
    assert!(
        out.contains("governance_connector_metrics_scrape_errors_total{reason=\"query_error\"} 0")
    );
}

#[tokio::test]
async fn a_db_outage_never_produces_a_healthy_looking_reading() {
    let metrics = Metrics::new();
    let pool = unreachable_pool();

    let start = Instant::now();
    metrics
        .refresh_connector_freshness(&pool, "tenant-outage", Duration::from_millis(200))
        .await;
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "must fail within the configured timeout, not sqlx's 30s pool default -- took \
         {elapsed:?}"
    );

    let out = metrics.render();
    assert!(
        !out.contains("governance_connector_last_success_timestamp_seconds"),
        "an outage must not fabricate a timestamp"
    );
    assert!(
        !out.contains("governance_connector_has_synced"),
        "an outage must not fabricate a has_synced reading either"
    );
    assert!(
        out.contains("governance_connector_metrics_scrape_errors_total{reason=\"timeout\"} 1"),
        "the outage must be visible via the error counter -- got:\n{out}"
    );
}

#[tokio::test]
async fn a_failed_refresh_leaves_a_previously_good_reading_in_place_rather_than_erasing_it() {
    // Once a value is known good, a later failed refresh must not erase
    // it back to "unknown" -- the timestamp is an immutable historical
    // fact (see the module doc comment on why this is safe, unlike a raw
    // age gauge). Simulated directly (no DB) by touching the gauges the
    // way a successful refresh would, then running a refresh that can
    // only fail. `unreachable_pool()` drives this through the `timeout`
    // path specifically (same as `a_db_outage_never_produces_a_healthy_looking_reading`
    // above) -- there is no distinct code path for "query_error" vs
    // "timeout" here, both `match` arms `return` before touching either
    // gauge, so exercising one proves the shared behaviour.
    let metrics = Metrics::new();
    metrics
        .connector_has_synced
        .with_label_values(&["github_copilot"])
        .set(1);
    metrics
        .connector_last_success_timestamp_seconds
        .with_label_values(&["github_copilot"])
        .set(1_700_000_000);

    let pool = unreachable_pool();
    metrics
        .refresh_connector_freshness(&pool, "tenant-outage-2", Duration::from_millis(200))
        .await;

    let out = metrics.render();
    assert!(
        out.contains(
            "governance_connector_last_success_timestamp_seconds{provider=\"github_copilot\"} \
             1700000000"
        ),
        "a failed refresh must not erase a previously observed timestamp -- got:\n{out}"
    );
    assert!(
        out.contains("governance_connector_has_synced{provider=\"github_copilot\"} 1"),
        "a failed refresh must not erase a previously observed has_synced -- got:\n{out}"
    );
}

/// End-to-end against a real database: a tenant that has never written a
/// single `ingest_manifests` row must render `has_synced=0` for the
/// known provider, and must NOT render a timestamp series at all --
/// proving the "never synced" state is visibly unhealthy, not the same
/// as a connector that just hasn't been asked about yet.
#[tokio::test]
async fn a_never_synced_tenant_renders_has_synced_zero_not_a_healthy_looking_gap() {
    let Some(pool) = connected_pool().await else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let metrics = Metrics::new();
    let tenant_id = format!("tenant-never-synced-{}", cuid::cuid2());

    metrics
        .refresh_connector_freshness(&pool, &tenant_id, Duration::from_secs(3))
        .await;

    let out = metrics.render();
    assert!(
        out.contains("governance_connector_has_synced{provider=\"github_copilot\"} 0"),
        "a never-synced provider must be explicitly reported as has_synced=0 -- got:\n{out}"
    );
    assert!(
        !out.contains("governance_connector_last_success_timestamp_seconds"),
        "a never-synced provider must not render a fabricated timestamp -- got:\n{out}"
    );
    assert!(
        out.contains("governance_connector_metrics_scrape_errors_total{reason=\"timeout\"} 0"),
        "a successful refresh against a real, reachable DB must not count as an error"
    );
}

/// End-to-end against a real database: a tenant with a manifest row for
/// today reports `has_synced=1` and a timestamp within the last day --
/// i.e. a small age once computed via `time() - metric` in PromQL,
/// proving the happy path actually renders a usable, close-to-now value
/// and not just "some number".
#[tokio::test]
async fn a_recent_successful_day_renders_has_synced_one_and_a_small_age() {
    let Some(pool) = connected_pool().await else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let metrics = Metrics::new();
    let tenant_id = format!("tenant-recent-{}", cuid::cuid2());
    let today = chrono::Utc::now().date_naive();

    cratestack::sqlx::query(
        "INSERT INTO ingest_manifests \
         (id, tenant_id, provider, scope_id, report_day, report_type, status, \
          record_count, schema_version, started_at, completed_at) \
         VALUES ($1, $2, 'github_copilot', 'scope', CAST($3 AS date), \
                 'organization-1-day', 'ok', 1, 1, now(), now())",
    )
    .bind(format!("manifest-{tenant_id}"))
    .bind(&tenant_id)
    .bind(today.to_string())
    .execute(&pool)
    .await
    .expect("insert manifest fixture");

    metrics
        .refresh_connector_freshness(&pool, &tenant_id, Duration::from_secs(3))
        .await;

    let out = metrics.render();
    assert!(
        out.contains("governance_connector_has_synced{provider=\"github_copilot\"} 1"),
        "a provider with a today-dated successful manifest must report has_synced=1 -- \
         got:\n{out}"
    );

    let expected_epoch = today
        .and_hms_opt(0, 0, 0)
        .expect("midnight is always a valid time")
        .and_utc()
        .timestamp();
    assert!(
        out.contains(&format!(
            "governance_connector_last_success_timestamp_seconds{{provider=\"github_copilot\"}} \
             {expected_epoch}"
        )),
        "must report the exact stored report_day as a unix timestamp -- got:\n{out}"
    );

    let age_seconds = chrono::Utc::now().timestamp() - expected_epoch;
    assert!(
        (0..86_400).contains(&age_seconds),
        "a report_day of today must compute to a small (< 24h) age via time() - metric, \
         got {age_seconds}s"
    );
}
