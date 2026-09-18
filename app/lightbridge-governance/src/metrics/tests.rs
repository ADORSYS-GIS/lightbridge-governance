//! Tests for [`super`]. Split into its own file (issue #175) rather than
//! raising the already-grandfathered LoC ceiling -- the same move
//! `otel/tests.rs` made, applied to the whole `mod tests` block.

use std::time::{Duration, Instant};

use cratestack::sqlx::PgPool;

use super::Metrics;

#[test]
fn counters_render_after_recording() {
    let metrics = Metrics::new();
    metrics
        .connector_metrics_scrape_errors_total
        .with_label_values(&["timeout"])
        .inc();

    let out = metrics.render();
    assert!(
        out.contains("governance_connector_metrics_scrape_errors_total{reason=\"timeout\"} 1"),
        "scrape error counter must render after incrementing -- got:\n{out}"
    );
}

#[test]
fn render_of_an_untouched_registry_is_well_formed() {
    // Smoke test of the render path with an untouched registry: the
    // Prometheus text format must not contain a NaN value (which would
    // mean a counter was left in a broken state), and the render call
    // itself must succeed.
    let out = Metrics::new().render();
    assert!(!out.contains("NaN"), "rendered output must not contain NaN");
}

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

/// A pool that can never connect (mirrors `resolve.rs`'s
/// `unreachable_state` technique) -- proves the DB-unavailable path
/// without needing a real Postgres to be down. No `#[expect]` needed:
/// this lives inside `#[cfg(test)] mod tests`, which `clippy.toml`'s
/// `allow-expect-in-tests` already covers (unlike a free-standing helper
/// in `tests/support/`, see `resolve.rs`'s own `unreachable_state()`,
/// which carries no suppression either).
fn unreachable_pool() -> PgPool {
    cratestack::sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://x:x@127.0.0.1:1/does-not-matter")
        .expect("lazy pool construction never actually connects")
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

/// Runs against a real Postgres when `DATABASE_URL` is set, mirroring
/// `resolve.rs`/`ingest.rs`'s own gated integration tests. Reports (via
/// `eprintln!`) rather than vanishing silently when skipped.
async fn connected_pool() -> Option<PgPool> {
    let database_url = std::env::var("DATABASE_URL").ok()?;
    let pool = PgPool::connect(&database_url).await.expect("connect");
    static MIGRATION_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    {
        let _guard = MIGRATION_LOCK.lock().await;
        governance_core::migrate::run(&pool).await.expect("migrate");
    }
    Some(pool)
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

/// End-to-end against a real database: exercises the full happy path --
/// active/engaged users, daily and month-to-date cost, seats assigned
/// and seats never used all render with the tenant's actual values, and
/// `has_data` flips to `1` for both families.
#[tokio::test]
async fn a_tenant_with_data_renders_the_full_org_kpi_family() {
    let Some(pool) = connected_pool().await else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let metrics = Metrics::new();
    let tenant_id = format!("tenant-org-kpi-full-{}", cuid::cuid2());
    let org = "org-e2e";
    // Two fixed (not "today") days in the same calendar month, so
    // daily cost and month-to-date cost are deliberately DIFFERENT
    // numbers -- this is what makes the two separate assertions below
    // actually load-bearing (with only one day of data the two values
    // would coincide, and a bug that swapped them would go undetected).
    let earlier_day = "2026-04-05";
    let latest_day = "2026-04-08";

    cratestack::sqlx::query(
        "INSERT INTO copilot_org_dailys \
         (id, tenant_id, organization_id, report_day, active_users, engaged_users, \
          total_interactions, code_generations, code_acceptances, loc_suggested, \
          loc_added, loc_deleted, ai_credits, net_cost_micro_usd) \
         VALUES ($1, $2, $3, CAST($4 AS date), 5, 2, 0, 0, 0, 0, 0, 0, 0, 1_000_000)",
    )
    .bind(format!("metrics-e2e-org-earlier:{tenant_id}"))
    .bind(&tenant_id)
    .bind(org)
    .bind(earlier_day)
    .execute(&pool)
    .await
    .expect("insert earlier org daily fixture");

    cratestack::sqlx::query(
        "INSERT INTO copilot_org_dailys \
         (id, tenant_id, organization_id, report_day, active_users, engaged_users, \
          total_interactions, code_generations, code_acceptances, loc_suggested, \
          loc_added, loc_deleted, ai_credits, net_cost_micro_usd) \
         VALUES ($1, $2, $3, CAST($4 AS date), 17, 9, 0, 0, 0, 0, 0, 0, 0, 4_500_000)",
    )
    .bind(format!("metrics-e2e-org:{tenant_id}"))
    .bind(&tenant_id)
    .bind(org)
    .bind(latest_day)
    .execute(&pool)
    .await
    .expect("insert org daily fixture");

    cratestack::sqlx::query(
        "INSERT INTO copilot_seat_snapshots \
         (id, tenant_id, organization_id, snapshot_day, provider_user_id, user_login, \
          seat_assigned_at, last_activity_at, last_activity_editor, seat_state) \
         VALUES ($1, $2, $3, CAST($4 AS date), 'user-used', 'user-used', now(), now(), \
                 NULL, 'active')",
    )
    .bind(format!("metrics-e2e-seat-used:{tenant_id}"))
    .bind(&tenant_id)
    .bind(org)
    .bind(latest_day)
    .execute(&pool)
    .await
    .expect("insert used seat fixture");

    cratestack::sqlx::query(
        "INSERT INTO copilot_seat_snapshots \
         (id, tenant_id, organization_id, snapshot_day, provider_user_id, user_login, \
          seat_assigned_at, last_activity_at, last_activity_editor, seat_state) \
         VALUES ($1, $2, $3, CAST($4 AS date), 'user-never-used', 'user-never-used', now(), \
                 NULL, NULL, 'active')",
    )
    .bind(format!("metrics-e2e-seat-unused:{tenant_id}"))
    .bind(&tenant_id)
    .bind(org)
    .bind(latest_day)
    .execute(&pool)
    .await
    .expect("insert never-used seat fixture");

    metrics
        .refresh_org_kpis(&pool, &tenant_id, Duration::from_secs(3))
        .await;

    let out = metrics.render();
    assert!(
        out.contains(&format!(
            "governance_org_active_users{{organization_id=\"{org}\"}} 17"
        )),
        "must report the LATEST day's active_users (17), not the earlier day's (5) -- \
         got:\n{out}"
    );
    assert!(out.contains(&format!(
        "governance_org_engaged_users{{organization_id=\"{org}\"}} 9"
    )));
    assert!(
        out.contains(&format!(
            "governance_org_daily_cost_micro_usd{{organization_id=\"{org}\"}} 4500000"
        )),
        "daily cost must be only the latest day's own cost (4_500_000), not summed with \
         the earlier day -- got:\n{out}"
    );
    assert!(
        out.contains(&format!(
            "governance_org_cost_month_to_date_micro_usd{{organization_id=\"{org}\"}} 5500000"
        )),
        "month-to-date must sum both days in the month (1_000_000 + 4_500_000 = \
         5_500_000), not just the latest day's own cost -- got:\n{out}"
    );
    assert!(out.contains(&format!(
        "governance_org_seats_assigned{{organization_id=\"{org}\"}} 2"
    )));
    assert!(out.contains(&format!(
        "governance_org_seats_never_used{{organization_id=\"{org}\"}} 1"
    )));
    assert!(out.contains("governance_org_kpi_has_data{family=\"usage\"} 1"));
    assert!(out.contains("governance_org_kpi_has_data{family=\"seats\"} 1"));
}
