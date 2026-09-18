use std::{collections::HashMap, time::Duration};

use cratestack::sqlx::PgPool;
use prometheus::{IntGaugeVec, Registry, opts};

use super::Metrics;

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

/// The `governance_connector_*` gauges, built and registered by [`build`] and
/// then moved into [`Metrics`]. Fields are `pub(super)` so the parent module
/// can assemble the struct; none is part of the public API.
pub(super) struct ConnectorGauges {
    pub(super) last_success_timestamp_seconds: IntGaugeVec,
    pub(super) has_synced: IntGaugeVec,
}

/// Constructs and registers the `governance_connector_*` family.
#[expect(
    clippy::expect_used,
    reason = "impossibility proof: metric construction only fails on duplicate names or \
              invalid help text, and both are compile-time string literals here"
)]
pub(super) fn build(registry: &Registry) -> ConnectorGauges {
    let last_success_timestamp_seconds = IntGaugeVec::new(
        opts!(
            "governance_connector_last_success_timestamp_seconds",
            "unix timestamp of the most recent successfully-ingested report day, by provider \
             (ADR-0007); absent, never 0, until a refresh has actually observed one"
        ),
        &["provider"],
    )
    .expect("static metric definition");
    let has_synced = IntGaugeVec::new(
        opts!(
            "governance_connector_has_synced",
            "1 if the provider has ever recorded a successful ingest_manifests row, 0 if a \
             refresh has confirmed it never has, absent if never yet determined (ADR-0007)"
        ),
        &["provider"],
    )
    .expect("static metric definition");
    super::register(
        registry,
        vec![
            Box::new(last_success_timestamp_seconds.clone()),
            Box::new(has_synced.clone()),
        ],
    );
    ConnectorGauges {
        last_success_timestamp_seconds,
        has_synced,
    }
}

impl Metrics {
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
}
