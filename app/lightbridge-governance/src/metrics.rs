//! Prometheus metrics for the ServiceMonitor (ADR-0007).
//!
//! Three kinds of metric live here:
//! - `governance_connector_*`, derived from `ingest_manifests` (ADR-0007).
//!   The query itself lives in `governance_core::connector_metrics` -- this
//!   module owns turning it into series, bounding it with a timeout, and
//!   deciding what a query failure looks like on `/metrics`.
//! - `governance_ingest_*` for the `/internal/v1/ingest` telemetry path, so an
//!   ingest outage (auth failures, malformed OTLP, storage errors, rate
//!   limiting) is observable, not a silent 500 in a log that nobody reads.
//! - `governance_org_*`, a small set of org-level KPI gauges (active/engaged
//!   users, cost, seats) derived from `copilot_org_dailys`/
//!   `copilot_seat_snapshots`, for alerting. The queries live in
//!   `governance_core::org_kpis` -- see that module's doc comment for why
//!   this is a deliberate, bounded exception to ADR-0003's "Mimir keeps only
//!   `governance_connector_*`", why it is derived here rather than pushed
//!   through the copilot-sync OTel collector (ADR-0011 is dashboard-grade,
//!   not alert-grade, for exactly the reason that would undermine an alert),
//!   and the absent-vs-zero contract these queries follow. This module's job
//!   is the same three things as for `governance_connector_*` above: turn
//!   the query result into series, bound it with a timeout, and decide what
//!   a query failure looks like on `/metrics` -- see
//!   [`Metrics::refresh_org_kpis`].
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
    /// Executions whose payload email contradicted the token-derived identity.
    pub ingest_identity_mismatches_total: IntCounter,
    /// Unix timestamp (seconds) of the most recent report day `{provider}`
    /// has successfully ingested at least one report. Absent for a
    /// provider that has never synced, or before the first successful
    /// refresh -- never `0`, which reads as "just synced". Backs the
    /// runbook's "no successful sync in 36h" / "report older than 72h"
    /// alerts via `time() - metric > threshold` in PromQL.
    connector_last_success_timestamp_seconds: IntGaugeVec,
    /// `1` once `{provider}` has EVER recorded a successful manifest row,
    /// `0` if a refresh has confirmed it never has. Absent only before the
    /// first successful refresh. This is what makes a freshly deployed
    /// connector (which has no timestamp to be stale) distinguishable from a
    /// healthy one -- see the module doc comment.
    connector_has_synced: IntGaugeVec,
    /// Failed `/metrics` refresh attempts against Postgres, by `reason`.
    /// Covers both `governance_connector_*` (`timeout`/`query_error`,
    /// ADR-0007) and `governance_org_*` (`org_usage_timeout`/
    /// `org_usage_query_error`/`org_seats_timeout`/`org_seats_query_error`)
    /// -- one shared counter rather than a second one, since both are "a
    /// `/metrics` scrape's Postgres refresh failed" and an operator watching
    /// for scrape-path trouble should not need to know which family to
    /// check. Always present, starting at `0` for every reason -- unlike the
    /// gauges themselves, "no failures yet" IS a safe default for a counter,
    /// so every reason is set to `0` at registration rather than left
    /// absent. An alert can watch `increase(...[10m]) > 0` as a
    /// belt-and-suspenders signal independent of the gauges being absent or
    /// stale.
    pub connector_metrics_scrape_errors_total: IntCounterVec,
    /// Active users on `{organization_id}`'s most recent AVAILABLE report
    /// day (ADR-0001 tenant_id is in the query's WHERE clause, never a
    /// label -- see `governance_core::org_kpis`). Absent until a refresh has
    /// actually observed a row for that organization.
    org_active_users: IntGaugeVec,
    /// Engaged users, same day/absence contract as `org_active_users`.
    org_engaged_users: IntGaugeVec,
    /// Integer micro-USD (ADR-0008): net cost on `{organization_id}`'s most
    /// recent available report day.
    org_daily_cost_micro_usd: IntGaugeVec,
    /// Integer micro-USD (ADR-0008): net cost summed from the first day of
    /// that report day's calendar month through the report day itself.
    org_cost_month_to_date_micro_usd: IntGaugeVec,
    /// Seats assigned as of `{organization_id}`'s most recent seat snapshot.
    org_seats_assigned: IntGaugeVec,
    /// Seats assigned as of the most recent snapshot with
    /// `last_activity_at IS NULL` -- the licence-waste signal. Same
    /// day/absence contract as the gauges above.
    org_seats_never_used: IntGaugeVec,
    /// `1` once a refresh has confirmed at least one row exists for this
    /// tenant in the `family` table (`usage` = `copilot_org_dailys`,
    /// `seats` = `copilot_seat_snapshots`; any organization, any day), `0`
    /// once a refresh has confirmed there are none, absent before that
    /// family's first successful refresh.
    ///
    /// Labeled by `family`, not `organization_id`: the question this
    /// answers -- "does this TENANT have any data at all" -- is meaningless
    /// per-organization, since an organization that has never reported
    /// cannot appear as an `organization_id` label value in the first place
    /// (see the module doc comment / `org_kpis`'s absent-vs-zero contract).
    /// `family` is a fixed two-value set, so this stays as bounded as a
    /// genuinely unlabeled gauge would be.
    ///
    /// Deliberately an `IntGaugeVec`, not a plain scalar `IntGauge`: a
    /// scalar gauge always renders (defaulting to `0`) the instant it is
    /// registered, which would make "confirmed zero" and "never yet
    /// refreshed" both read as `0` -- exactly the ambiguity this gauge
    /// exists to remove. `IntGaugeVec` only materializes a series once
    /// `with_label_values(...)` is actually called, matching
    /// `governance_connector_has_synced`'s own absent-until-touched
    /// mechanism.
    org_kpi_has_data: IntGaugeVec,
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
        let ingest_identity_mismatches_total = IntCounter::new(
            "governance_ingest_identity_mismatches_total",
            "executions whose payload email contradicted the token-derived identity",
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
                "failed /metrics Postgres refresh attempts, by reason -- covers both \
                 governance_connector_* (ADR-0007) and governance_org_* (org-level KPI gauges)"
            ),
            &["reason"],
        )
        .expect("static metric definition");
        let org_active_users = IntGaugeVec::new(
            opts!(
                "governance_org_active_users",
                "active users on the organization's most recent AVAILABLE report day; absent \
                 until a refresh has observed a row for that organization"
            ),
            &["organization_id"],
        )
        .expect("static metric definition");
        let org_engaged_users = IntGaugeVec::new(
            opts!(
                "governance_org_engaged_users",
                "engaged users on the organization's most recent AVAILABLE report day; same \
                 absence contract as governance_org_active_users"
            ),
            &["organization_id"],
        )
        .expect("static metric definition");
        let org_daily_cost_micro_usd = IntGaugeVec::new(
            opts!(
                "governance_org_daily_cost_micro_usd",
                "net Copilot cost, integer micro-USD (ADR-0008), on the organization's most \
                 recent AVAILABLE report day -- estimated, not reconciled invoiced spend"
            ),
            &["organization_id"],
        )
        .expect("static metric definition");
        let org_cost_month_to_date_micro_usd = IntGaugeVec::new(
            opts!(
                "governance_org_cost_month_to_date_micro_usd",
                "net Copilot cost, integer micro-USD (ADR-0008), summed from the first day of \
                 the most recent available report day's calendar month through that day"
            ),
            &["organization_id"],
        )
        .expect("static metric definition");
        let org_seats_assigned = IntGaugeVec::new(
            opts!(
                "governance_org_seats_assigned",
                "seats assigned as of the organization's most recent seat snapshot"
            ),
            &["organization_id"],
        )
        .expect("static metric definition");
        let org_seats_never_used = IntGaugeVec::new(
            opts!(
                "governance_org_seats_never_used",
                "seats assigned as of the most recent snapshot with last_activity_at IS NULL -- \
                 the licence-waste signal"
            ),
            &["organization_id"],
        )
        .expect("static metric definition");
        let org_kpi_has_data = IntGaugeVec::new(
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

        let metrics = Self {
            registry: Registry::new(),
            ingest_requests_total: ingest_requests_total.clone(),
            ingest_executions_total: ingest_executions_total.clone(),
            ingest_model_calls_total: ingest_model_calls_total.clone(),
            ingest_tool_calls_total: ingest_tool_calls_total.clone(),
            ingest_identity_mismatch_failures_total: ingest_identity_mismatch_failures_total
                .clone(),
            ingest_identity_mismatches_total: ingest_identity_mismatches_total.clone(),
            connector_last_success_timestamp_seconds: connector_last_success_timestamp_seconds
                .clone(),
            connector_has_synced: connector_has_synced.clone(),
            connector_metrics_scrape_errors_total: connector_metrics_scrape_errors_total.clone(),
            org_active_users: org_active_users.clone(),
            org_engaged_users: org_engaged_users.clone(),
            org_daily_cost_micro_usd: org_daily_cost_micro_usd.clone(),
            org_cost_month_to_date_micro_usd: org_cost_month_to_date_micro_usd.clone(),
            org_seats_assigned: org_seats_assigned.clone(),
            org_seats_never_used: org_seats_never_used.clone(),
            org_kpi_has_data: org_kpi_has_data.clone(),
        };

        // Registry::register fails only on a name collision or an already
        // registered collector -- impossible here since each is registered
        // exactly once. Logged, not fatal: a missing metric is worse than a
        // 500 on startup.
        let collectors: [Box<dyn prometheus::core::Collector>; 16] = [
            Box::new(ingest_requests_total),
            Box::new(ingest_executions_total),
            Box::new(ingest_model_calls_total),
            Box::new(ingest_tool_calls_total),
            Box::new(ingest_identity_mismatch_failures_total),
            Box::new(ingest_identity_mismatches_total),
            Box::new(connector_last_success_timestamp_seconds),
            Box::new(connector_has_synced),
            Box::new(connector_metrics_scrape_errors_total),
            Box::new(org_active_users),
            Box::new(org_engaged_users),
            Box::new(org_daily_cost_micro_usd),
            Box::new(org_cost_month_to_date_micro_usd),
            Box::new(org_seats_assigned),
            Box::new(org_seats_never_used),
            Box::new(org_kpi_has_data),
        ];
        for collector in collectors {
            if let Err(error) = metrics.registry.register(collector) {
                tracing::warn!(error = %error, "metric registration failed");
            }
        }

        // "No failures yet" is a legitimate, non-misleading default for a
        // counter (unlike the freshness/KPI gauges) -- initialize every
        // reason to 0 so the series exists from process start rather than
        // only appearing the first time something actually fails.
        for reason in [
            "timeout",
            "query_error",
            "org_usage_timeout",
            "org_usage_query_error",
            "org_seats_timeout",
            "org_seats_query_error",
        ] {
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
        self.refresh_org_seat_kpis(pool, tenant_id, timeout).await;
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

    async fn refresh_org_seat_kpis(&self, pool: &PgPool, tenant_id: &str, timeout: Duration) {
        let outcome = tokio::time::timeout(
            timeout,
            governance_core::org_kpis::org_seat_kpis(pool, tenant_id),
        )
        .await;

        let rows = match outcome {
            Ok(Ok(rows)) => rows,
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "governance_org_* seats refresh: query failed");
                self.connector_metrics_scrape_errors_total
                    .with_label_values(&["org_seats_query_error"])
                    .inc();
                return;
            }
            Err(_elapsed) => {
                tracing::warn!(
                    timeout_ms = timeout.as_millis(),
                    "governance_org_* seats refresh: timed out"
                );
                self.connector_metrics_scrape_errors_total
                    .with_label_values(&["org_seats_timeout"])
                    .inc();
                return;
            }
        };

        self.org_kpi_has_data
            .with_label_values(&["seats"])
            .set(i64::from(!rows.is_empty()));

        for row in &rows {
            self.org_seats_assigned
                .with_label_values(&[&row.organization_id])
                .set(row.seats_assigned);
            self.org_seats_never_used
                .with_label_values(&[&row.organization_id])
                .set(row.seats_never_used);
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
        metrics.ingest_identity_mismatches_total.inc();

        let out = metrics.render();
        assert!(out.contains("governance_ingest_requests_total{outcome=\"success\"} 1"));
        assert!(out.contains("governance_ingest_executions_total 1"));
        assert!(out.contains("governance_ingest_identity_mismatch_failures_total 1"));
        assert!(out.contains("governance_ingest_identity_mismatches_total 1"));
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
}
