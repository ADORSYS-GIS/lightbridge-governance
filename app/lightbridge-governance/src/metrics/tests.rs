mod connector;
mod org_absence;
mod org_happy_path;

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
