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

use prometheus::Registry;

mod connector;
mod org;
mod registry;

#[cfg(test)]
mod tests;

pub use registry::Metrics;

/// Registers a set of collectors into `registry`, logging (not failing) on a
/// name collision or duplicate registration -- a missing metric is worse than
/// a 500 on startup. `Registry::register` fails only in those two cases, and
/// each collector here is registered exactly once, so this is best-effort.
fn register(registry: &Registry, collectors: Vec<Box<dyn prometheus::core::Collector>>) {
    for collector in collectors {
        if let Err(error) = registry.register(collector) {
            tracing::warn!(error = %error, "metric registration failed");
        }
    }
}
