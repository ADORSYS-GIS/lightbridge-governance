//! Prometheus metrics for the ServiceMonitor (ADR-0007).
//!
//! Three kinds of metric live here:
//! - `governance_connector_*`, derived from `ingest_manifests` (ADR-0007).
//!   The query itself lives in `governance_core::connector_metrics` -- this
//!   module owns turning it into series, bounding it with a timeout, and
//!   deciding what a query failure looks like on `/metrics`.
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
use prometheus::{IntCounterVec, IntGaugeVec, Registry, opts};

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
        let collectors: [Box<dyn prometheus::core::Collector>; 10] = [
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
mod tests;
