//! Which OTLP signal a payload carries.
//!
//! The daemon accepts any path (Codex posts `POST /` verbatim with a
//! `resourceLogs` body), so for a **JSON** body the URL path cannot tell us
//! metrics from logs — the body must. OTLP/HTTP JSON names the signal by key
//! presence: `resourceMetrics` -> metrics, `resourceLogs` -> logs. An
//! unrecognised body defaults to logs, the common case (Codex and Claude Code
//! both emit logs; only an explicit metrics exporter emits metrics).
//!
//! For a **non-JSON** (protobuf) body the content can't be inspected cheaply, so
//! the signal is taken from the appended URL path (`/v1/metrics` -> metrics) —
//! a real exporter always appends its signal path, and the body correctly
//! arriving beats a JSON-only reading of it.

use serde_json::Value;

use crate::copilot::Signal;

/// Detects the OTLP signal in a payload, falling back to the URL path when the
/// body is not JSON.
///
/// For a **JSON** body the signal is named by key presence (`resourceMetrics` ->
/// metrics; anything else, including logs and traces, -> logs). For a
/// **non-JSON** body (OTLP protobuf) the body cannot be inspected cheaply, so the
/// signal is taken from the URL path, which a real exporter appends
/// (`/v1/metrics` -> metrics); an unrecognised path defaults to logs, the common
/// case. This keeps a protobuf metrics exporter from being misdelivered to
/// `/v1/logs`.
pub fn signal(body: &[u8], path: &str) -> Signal {
    if let Ok(value) = serde_json::from_slice::<Value>(body) {
        return if value.get("resourceMetrics").is_some() {
            Signal::Metrics
        } else {
            Signal::Logs
        };
    }
    if path.ends_with("/v1/metrics") {
        Signal::Metrics
    } else {
        Signal::Logs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_metrics_body_detects_metrics() {
        let body = br#"{"resourceMetrics":[{"resource":{}}]}"#;
        assert_eq!(signal(body, "/"), Signal::Metrics);
    }

    #[test]
    fn a_logs_body_detects_logs() {
        let body = br#"{"resourceLogs":[{"resource":{}}]}"#;
        assert_eq!(signal(body, "/"), Signal::Logs);
    }

    #[test]
    fn a_body_naming_neither_defaults_to_logs() {
        let body = br#"{"resourceTraces":[{"resource":{}}]}"#;
        assert_eq!(signal(body, "/"), Signal::Logs);
    }

    #[test]
    fn an_empty_body_defaults_to_logs() {
        assert_eq!(signal(b"", "/"), Signal::Logs);
    }

    #[test]
    fn a_non_json_body_uses_the_metrics_path() {
        // OTLP protobuf metrics: the body can't be inspected, so the appended
        // URL path must route it to /v1/metrics rather than misdelivering to logs.
        assert_eq!(signal(b"\x0a\x03\x12\x04", "/v1/metrics"), Signal::Metrics);
        assert_eq!(signal(b"\x0a\x03\x12\x04", "/v1/logs"), Signal::Logs);
        assert_eq!(signal(b"\x0a\x03\x12\x04", "/"), Signal::Logs);
    }

    #[test]
    fn a_non_json_body_on_an_unknown_path_defaults_to_logs() {
        assert_eq!(signal(b"\x0a\x03\x12\x04", "/garbage"), Signal::Logs);
    }
}
