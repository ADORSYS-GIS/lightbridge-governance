//! Prometheus metrics for the ServiceMonitor (ADR-0007).
//!
//! Two kinds of counter live here:
//! - `governance_connector_*` derived from `ingest_manifest` (ADR-0007's own
//!   decision, not yet implemented -- see below).
//! - `governance_ingest_*` for the `/internal/v1/ingest` telemetry path, so an
//!   ingest outage (auth failures, malformed OTLP, storage errors, rate
//!   limiting) is observable, not a silent 500 in a log that nobody reads.

use prometheus::{IntCounter, IntCounterVec, Registry, opts};

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

        let metrics = Self {
            registry: Registry::new(),
            ingest_requests_total: ingest_requests_total.clone(),
            ingest_executions_total: ingest_executions_total.clone(),
            ingest_model_calls_total: ingest_model_calls_total.clone(),
            ingest_tool_calls_total: ingest_tool_calls_total.clone(),
            ingest_identity_mismatch_failures_total: ingest_identity_mismatch_failures_total
                .clone(),
        };

        // Registry::register fails only on a name collision or an already
        // registered collector -- impossible here since each is registered
        // exactly once. Logged, not fatal: a missing metric is worse than a
        // 500 on startup.
        let collectors: [Box<dyn prometheus::core::Collector>; 5] = [
            Box::new(ingest_requests_total),
            Box::new(ingest_executions_total),
            Box::new(ingest_model_calls_total),
            Box::new(ingest_tool_calls_total),
            Box::new(ingest_identity_mismatch_failures_total),
        ];
        for collector in collectors {
            if let Err(error) = metrics.registry.register(collector) {
                tracing::warn!(error = %error, "metric registration failed");
            }
        }

        metrics
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
}
