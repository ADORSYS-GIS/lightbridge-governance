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
/// `parsed` is the body already parsed once by the caller (`None` when it is
/// not JSON) -- taken rather than parsing here, since the caller needs its
/// own parse anyway (for [`super::normalize::stamp`]) and re-parsing the same
/// bytes was one of three redundant `serde_json::from_slice` calls per
/// request (#290 review round 2).
///
/// For a **JSON** body the signal is named by key presence (`resourceMetrics` ->
/// metrics; anything else, including logs and traces, -> logs). For a
/// **non-JSON** body (OTLP protobuf) the body cannot be inspected cheaply, so the
/// signal is taken from the URL path, which a real exporter appends
/// (`/v1/metrics` -> metrics); an unrecognised path defaults to logs, the common
/// case. This keeps a protobuf metrics exporter from being misdelivered to
/// `/v1/logs`.
pub fn signal(parsed: Option<&Value>, path: &str) -> Signal {
    if let Some(value) = parsed {
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

    /// Parses `body` once and calls `signal` -- matches the new (caller
    /// parses once) signature without rewriting every call site below.
    fn signal_body(body: &[u8], path: &str) -> Signal {
        signal(serde_json::from_slice::<Value>(body).ok().as_ref(), path)
    }

    #[test]
    fn a_metrics_body_detects_metrics() {
        let body = br#"{"resourceMetrics":[{"resource":{}}]}"#;
        assert_eq!(signal_body(body, "/"), Signal::Metrics);
    }

    #[test]
    fn a_logs_body_detects_logs() {
        let body = br#"{"resourceLogs":[{"resource":{}}]}"#;
        assert_eq!(signal_body(body, "/"), Signal::Logs);
    }

    #[test]
    fn a_body_naming_neither_defaults_to_logs() {
        let body = br#"{"resourceTraces":[{"resource":{}}]}"#;
        assert_eq!(signal_body(body, "/"), Signal::Logs);
    }

    #[test]
    fn an_empty_body_defaults_to_logs() {
        assert_eq!(signal_body(b"", "/"), Signal::Logs);
    }

    #[test]
    fn a_non_json_body_uses_the_metrics_path() {
        // OTLP protobuf metrics: the body can't be inspected, so the appended
        // URL path must route it to /v1/metrics rather than misdelivering to logs.
        assert_eq!(
            signal_body(b"\x0a\x03\x12\x04", "/v1/metrics"),
            Signal::Metrics
        );
        assert_eq!(signal_body(b"\x0a\x03\x12\x04", "/v1/logs"), Signal::Logs);
        assert_eq!(signal_body(b"\x0a\x03\x12\x04", "/"), Signal::Logs);
    }

    #[test]
    fn a_non_json_body_on_an_unknown_path_defaults_to_logs() {
        assert_eq!(signal_body(b"\x0a\x03\x12\x04", "/garbage"), Signal::Logs);
    }
}
