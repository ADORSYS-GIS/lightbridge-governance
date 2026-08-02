//! Prometheus metrics.
//!
//! Same counter set and names as `redact-gateway`'s `metrics.rs` — this
//! service replaces it (ADR-0116), and the governance dashboards already key
//! on the `redact_*` names, so a rename here would silently blank them out
//! for no benefit. Deliberately low-cardinality: no per-user, per-model or
//! per-entity-value labels, and an entity *value* must never reach one.

use std::{net::SocketAddr, sync::Arc};

use axum::{Router, response::IntoResponse, routing::get};
use prometheus::{IntCounter, Registry};

/// The counters this service exports.
pub struct Metrics {
    registry: Registry,
    /// Requests (or streamed responses) received.
    pub requests_total: IntCounter,
    /// Spans rewritten across request and response bodies.
    pub redactions_total: IntCounter,
    /// Requests or responses refused because content matched a blocking rule.
    pub blocked_total: IntCounter,
    /// Requests refused because redaction could not be completed (fail-closed).
    ///
    /// ⚠️ The alerting signal that matters most: this going non-zero means the
    /// detector is failing and traffic is being refused.
    pub refused_total: IntCounter,
    /// Times a non-fail-closed profile continued past an indeterminate result.
    pub fail_open_total: IntCounter,
    /// Text fields examined.
    pub scanned_fields_total: IntCounter,
    /// Bodies whose shape yielded no recognised text fields.
    ///
    /// Non-zero means traffic is passing through uninspected — a shape we do
    /// not know, not necessarily a clean one. See ADR-0116: this is the
    /// counter that catches AIEG rewriting a body into a non-OpenAI schema.
    pub uninspected_total: IntCounter,
}

impl Metrics {
    /// Registers the counters.
    ///
    /// # Errors
    ///
    /// Returns an error if a counter fails to register, which would mean a
    /// duplicate name.
    pub fn new() -> prometheus::Result<Self> {
        let registry = Registry::new();

        macro_rules! counter {
            ($name:expr, $help:expr) => {{
                let c = IntCounter::new($name, $help)?;
                registry.register(Box::new(c.clone()))?;
                c
            }};
        }

        Ok(Self {
            requests_total: counter!("redact_requests_total", "Requests received."),
            redactions_total: counter!("redact_redactions_total", "Spans rewritten."),
            blocked_total: counter!(
                "redact_blocked_total",
                "Requests or responses refused for prohibited content."
            ),
            refused_total: counter!(
                "redact_refused_total",
                "Requests refused because redaction could not be completed."
            ),
            fail_open_total: counter!(
                "redact_fail_open_total",
                "Indeterminate results allowed through on a non-fail-closed profile."
            ),
            scanned_fields_total: counter!("redact_scanned_fields_total", "Text fields examined."),
            uninspected_total: counter!(
                "redact_uninspected_bodies_total",
                "Bodies with no recognised text fields."
            ),
            registry,
        })
    }

    /// Renders the registry in Prometheus text format.
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

/// Serves `/metrics` and `/livez` on `addr` until the process exits.
///
/// A separate HTTP listener from the gRPC one: the `ext_proc` service on
/// `LISTEN_ADDR` speaks nothing but gRPC, and Envoy's health/scrape story
/// should not depend on that protocol.
///
/// # Errors
///
/// Returns an error if the listener cannot bind or the server fails.
pub async fn serve(addr: SocketAddr, metrics: Arc<Metrics>) -> anyhow::Result<()> {
    let app = Router::new().route("/livez", get(|| async { "ok" })).route(
        "/metrics",
        get(move || {
            let metrics = Arc::clone(&metrics);
            async move { render(&metrics).into_response() }
        }),
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn render(metrics: &Metrics) -> String {
    metrics.render()
}

#[cfg(test)]
mod tests {
    use super::Metrics;

    #[test]
    fn counters_register_and_render() {
        let m = Metrics::new().expect("metrics");
        m.requests_total.inc();
        m.blocked_total.inc_by(3);
        let out = m.render();
        assert!(out.contains("redact_requests_total 1"), "{out}");
        assert!(out.contains("redact_blocked_total 3"), "{out}");
    }

    #[test]
    fn no_entity_values_are_exported() {
        let m = Metrics::new().expect("metrics");
        let out = m.render();
        assert!(
            !out.contains('{'),
            "metrics gained labels; verify no entity value can reach one:\n{out}"
        );
    }
}
