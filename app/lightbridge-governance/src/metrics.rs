//! Prometheus metrics for the ServiceMonitor (ADR-0007).
//!
//! Empty for now: deriving `governance_connector_*` from `ingest_manifest` is
//! ADR-0007's own decision, not yet implemented. This exists so `/metrics` is
//! a real, scrapeable endpoint rather than a 404 -- the ServiceMonitor this
//! chart adds needs somewhere true to point at.

pub struct Metrics {
    registry: prometheus::Registry,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            registry: prometheus::Registry::new(),
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
    use super::Metrics;

    #[test]
    fn empty_registry_still_renders_valid_output() {
        let out = Metrics::new().render();
        assert_eq!(out, "", "no counters registered yet, so nothing to render");
    }
}
