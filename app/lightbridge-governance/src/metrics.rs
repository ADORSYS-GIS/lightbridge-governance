//! Prometheus metrics for the ServiceMonitor (ADR-0007).
//!
//! Two kinds of metric live here:
//! - `governance_connector_*`, derived from `ingest_manifests` (ADR-0007).
//!   The query itself lives in `governance_core::connector_metrics` -- this
//!   module owns turning it into series, bounding it with a timeout, and
//!   deciding what a query failure looks like on `/metrics`.
//! - `governance_ingest_*` for the `/internal/v1/ingest` telemetry path, so an
//!   ingest outage (auth failures, malformed OTLP, storage errors, rate
//!   limiting) is observable, not a silent 500 in a log that nobody reads.
//!
//! ## `governance_connector_*`: refresh-on-scrape, not a background poller
//!
//! Two ways to keep this family current: recompute it on every `/metrics`
//! scrape, or run a periodic background task and always serve the
//! last-computed value. This picks the former. The ServiceMonitor scrapes
//! every 30s (`charts/lightbridge-governance/values.yaml`'s
//! `serviceMonitor.interval`) and the query is a single indexed aggregate
//! (`GROUP BY provider` over a handful of rows for a single-tenant
//! deployment), so recomputing it costs one cheap round-trip per scrape --
//! there is no meaningful "hammering" to avoid, and no second task,
//! shutdown-ordering, or staleness window to reason about. The cost is that a
//! slow/unreachable Postgres adds up to `timeout` of latency to the scrape
//! itself; that is bounded (see [`Metrics::refresh_connector_freshness`]) and
//! well under the ServiceMonitor's own scrape timeout budget.
//!
//! ## What a DB outage looks like on `/metrics`
//!
//! `governance_connector_last_success_timestamp_seconds` and
//! `governance_connector_has_synced` are **not** touched when a refresh
//! fails (timeout or query error) -- they keep whatever value they last held
//! (or stay absent, if none was ever observed), and
//! `governance_connector_metrics_scrape_errors_total` increments instead.
//! This is deliberate, not an oversight: freezing the *timestamp* during an
//! outage is safe, because it is an immutable historical fact ("the last day
//! we know succeeded was X") that does not become false just because we
//! cannot currently confirm it -- `time() - metric` in PromQL still computes
//! the correct, growing age against the real clock. Freezing a raw "age in
//! seconds" gauge instead would NOT be safe: it would stop advancing the
//! moment the outage starts and every subsequent scrape would report a
//! smaller age than reality, i.e. exactly the "stale-but-plausible value
//! that reads as fine" this feature exists to prevent. That is why this
//! module exposes a timestamp, not a raw age, and leaves age computation to
//! PromQL (`time() - governance_connector_last_success_timestamp_seconds`).
//! A connector that has never synced (or one Postgres cannot currently be
//! asked about) reports no timestamp series at all -- absent, not zero --
//! plus `governance_connector_has_synced == 0` once it is actually known to
//! be zero, so "unknown" is never misread as "zero seconds ago".

use std::{collections::HashMap, time::Duration};

use cratestack::sqlx::PgPool;
use prometheus::{IntCounter, IntCounterVec, IntGaugeVec, Registry, opts};

/// Provider strings this family covers today. `connector_freshness` already
/// discovers providers dynamically from `ingest_manifests` (`GROUP BY
/// provider`), but a provider with literally zero manifest rows cannot
/// appear in that grouped result at all -- there is nothing to group. This
/// list exists solely so a never-synced provider still gets an explicit
/// `has_synced=0`, rather than being indistinguishable from "no connectors
/// exist" (the exact failure mode this feature exists to close). Matches the
/// literal `"github_copilot"` `provider` string
/// `governance_copilot::sync::ingest_one` writes -- there is no shared
/// exported constant for it upstream (out of scope here: `crates/governance-copilot`).
const KNOWN_PROVIDERS: &[&str] = &["github_copilot"];

pub struct Metrics {
    registry: Registry,
    /// Total requests to `/internal/v1/ingest`, keyed by outcome. `total` is
    /// used by Prometheus rate() dashboards, so the counter must never reset
    /// across a redeploy in a way that corrupts the series -- the same
    /// counter is registered once at startup, never rebuilt.
    pub ingest_requests_total: IntCounterVec,
    /// Executions, model calls and tool calls persisted by ingest.
    pub ingest_executions_total: IntCounter,
    pub ingest_model_calls_total: IntCounter,
    pub ingest_tool_calls_total: IntCounter,
    /// Identity mismatch detection failures (best-effort query failures).
    pub ingest_identity_mismatch_failures_total: IntCounter,
    /// Unix timestamp (seconds) of the most recent report day `{provider}`
    /// has successfully ingested at least one report for. Absent for a
    /// provider that has never synced, or before the first successful
    /// refresh -- never `0`, which would misread as "just synced". Backs the
    /// runbook's "no successful sync in 36h" / "report older than 72h"
    /// alerts via `time() - metric > threshold` in PromQL.
    connector_last_success_timestamp_seconds: IntGaugeVec,
    /// `1` once `{provider}` has EVER recorded a successful manifest row,
    /// `0` if a refresh has confirmed it never has. Absent only before the
    /// first successful refresh. This is what makes a freshly deployed
    /// connector (which has no timestamp to be stale) distinguishable from a
    /// healthy one -- see the module doc comment.
    connector_has_synced: IntGaugeVec,
    /// Failed `governance_connector_*` refresh attempts, by `reason`
    /// (`timeout` or `query_error`). Always present, starting at `0` --
    /// unlike the two gauges above, "no failures yet" IS a safe default for
    /// a counter, so this one is set to `0` at registration rather than left
    /// absent. An alert can watch `increase(...[10m]) > 0` as a
    /// belt-and-suspenders signal independent of the freshness gauges being
    /// absent or stale.
    pub connector_metrics_scrape_errors_total: IntCounterVec,
}

impl Metrics {
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "impossibility proof: metric construction only fails on duplicate names or \
                  invalid help text, and both are compile-time string literals here"
    )]
    pub fn new() -> Self {
        let ingest_requests_total = IntCounterVec::new(
            opts!(
                "governance_ingest_requests_total",
                "ingest requests by outcome"
            ),
            &["outcome"],
        )
        .expect("static metric definition, name/help are fixed strings");
        let ingest_executions_total = IntCounter::new(
            "governance_ingest_executions_total",
            "executions upserted by /internal/v1/ingest",
        )
        .expect("static metric definition");
        let ingest_model_calls_total = IntCounter::new(
            "governance_ingest_model_calls_total",
            "model calls upserted by /internal/v1/ingest",
        )
        .expect("static metric definition");
        let ingest_tool_calls_total = IntCounter::new(
            "governance_ingest_tool_calls_total",
            "tool calls upserted by /internal/v1/ingest",
        )
        .expect("static metric definition");
        let ingest_identity_mismatch_failures_total = IntCounter::new(
            "governance_ingest_identity_mismatch_failures_total",
            "identity mismatch detection failures (best-effort query failures)",
        )
        .expect("static metric definition");
        let connector_last_success_timestamp_seconds = IntGaugeVec::new(
            opts!(
                "governance_connector_last_success_timestamp_seconds",
                "unix timestamp of the most recent successfully-ingested report day, by provider \
                 (ADR-0007); absent, never 0, until a refresh has actually observed one"
            ),
            &["provider"],
        )
        .expect("static metric definition");
        let connector_has_synced = IntGaugeVec::new(
            opts!(
                "governance_connector_has_synced",
                "1 if the provider has ever recorded a successful ingest_manifests row, 0 if a \
                 refresh has confirmed it never has, absent if never yet determined (ADR-0007)"
            ),
            &["provider"],
        )
        .expect("static metric definition");
        let connector_metrics_scrape_errors_total = IntCounterVec::new(
            opts!(
                "governance_connector_metrics_scrape_errors_total",
                "failed governance_connector_* refresh attempts against ingest_manifests, by \
                 reason (ADR-0007)"
            ),
            &["reason"],
        )
        .expect("static metric definition");

        let metrics = Self {
            registry: Registry::new(),
            ingest_requests_total: ingest_requests_total.clone(),
            ingest_executions_total: ingest_executions_total.clone(),
            ingest_model_calls_total: ingest_model_calls_total.clone(),
            ingest_tool_calls_total: ingest_tool_calls_total.clone(),
            ingest_identity_mismatch_failures_total: ingest_identity_mismatch_failures_total
                .clone(),
            connector_last_success_timestamp_seconds: connector_last_success_timestamp_seconds
                .clone(),
            connector_has_synced: connector_has_synced.clone(),
            connector_metrics_scrape_errors_total: connector_metrics_scrape_errors_total.clone(),
        };

        // Registry::register fails only on a name collision or an already
        // registered collector -- impossible here since each is registered
        // exactly once. Logged, not fatal: a missing metric is worse than a
        // 500 on startup.
        let collectors: [Box<dyn prometheus::core::Collector>; 8] = [
            Box::new(ingest_requests_total),
            Box::new(ingest_executions_total),
            Box::new(ingest_model_calls_total),
            Box::new(ingest_tool_calls_total),
            Box::new(ingest_identity_mismatch_failures_total),
            Box::new(connector_last_success_timestamp_seconds),
            Box::new(connector_has_synced),
            Box::new(connector_metrics_scrape_errors_total),
        ];
        for collector in collectors {
            if let Err(error) = metrics.registry.register(collector) {
                tracing::warn!(error = %error, "metric registration failed");
            }
        }

        // "No failures yet" is a legitimate, non-misleading default for a
        // counter (unlike the freshness gauges above) -- initialize both
        // reasons to 0 so the series exists from process start rather than
        // only appearing the first time something actually fails.
        for reason in ["timeout", "query_error"] {
            metrics
                .connector_metrics_scrape_errors_total
                .with_label_values(&[reason]);
        }

        metrics
    }

    /// Refreshes `governance_connector_*` from `ingest_manifests`
    /// (ADR-0007), bounded by `timeout` so a slow or unreachable Postgres
    /// cannot hang the `/metrics` scrape (see the module doc comment for why
    /// this runs on every scrape rather than on a background interval, and
    /// for exactly what a failure does and does not change).
    pub async fn refresh_connector_freshness(
        &self,
        pool: &PgPool,
        tenant_id: &str,
        timeout: Duration,
    ) {
        let outcome = tokio::time::timeout(
            timeout,
            governance_core::connector_metrics::connector_freshness(pool, tenant_id),
        )
        .await;

        let rows = match outcome {
            Ok(Ok(rows)) => rows,
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "governance_connector_* refresh: query failed");
                self.connector_metrics_scrape_errors_total
                    .with_label_values(&["query_error"])
                    .inc();
                return;
            }
            Err(_elapsed) => {
                tracing::warn!(
                    timeout_ms = timeout.as_millis(),
                    "governance_connector_* refresh: timed out"
                );
                self.connector_metrics_scrape_errors_total
                    .with_label_values(&["timeout"])
                    .inc();
                return;
            }
        };

        let by_provider: HashMap<&str, i64> = rows
            .iter()
            .map(|row| (row.provider.as_str(), row.last_success_at.timestamp()))
            .collect();

        for provider in KNOWN_PROVIDERS {
            match by_provider.get(provider) {
                Some(&last_success_epoch_seconds) => {
                    self.connector_has_synced
                        .with_label_values(&[provider])
                        .set(1);
                    self.connector_last_success_timestamp_seconds
                        .with_label_values(&[provider])
                        .set(last_success_epoch_seconds);
                }
                // Deliberately do NOT touch `connector_last_success_timestamp_seconds`
                // here: it must stay absent (never a fabricated 0) for a
                // provider that has never synced -- see the module doc
                // comment.
                None => {
                    self.connector_has_synced
                        .with_label_values(&[provider])
                        .set(0);
                }
            }
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        use prometheus::Encoder;
        let encoder = prometheus::TextEncoder::new();
        let mut buf = Vec::new();
        if encoder.encode(&self.registry.gather(), &mut buf).is_err() {
            return String::new();
        }
        String::from_utf8(buf).unwrap_or_default()
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use cratestack::sqlx::PgPool;

    use super::Metrics;

    #[test]
    fn counters_render_after_recording() {
        let metrics = Metrics::new();
        metrics
            .ingest_requests_total
            .with_label_values(&["success"])
            .inc();
        metrics.ingest_executions_total.inc();
        metrics.ingest_identity_mismatch_failures_total.inc();

        let out = metrics.render();
        assert!(out.contains("governance_ingest_requests_total{outcome=\"success\"} 1"));
        assert!(out.contains("governance_ingest_executions_total 1"));
        assert!(out.contains("governance_ingest_identity_mismatch_failures_total 1"));
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
        assert!(
            out.contains("governance_connector_metrics_scrape_errors_total{reason=\"timeout\"} 0")
        );
        assert!(out.contains(
            "governance_connector_metrics_scrape_errors_total{reason=\"query_error\"} 0"
        ));
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
}
