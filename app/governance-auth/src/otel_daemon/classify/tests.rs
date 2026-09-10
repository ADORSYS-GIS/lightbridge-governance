use opentelemetry_proto::tonic::{
    collector::{
        logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest,
        trace::v1::ExportTraceServiceRequest,
    },
    logs::v1::{LogRecord, ResourceLogs, ScopeLogs},
    metrics::v1::{Metric, ResourceMetrics, ScopeMetrics, Sum, metric::Data},
    trace::v1::{ResourceSpans, ScopeSpans, Span},
};
use prost::Message;

use super::*;

#[test]
fn binary_metrics_at_root_are_not_logs() {
    let body = b"\x0a\x09\x12\x07\x12\x05\x0a\x01m\x3a\x00";
    assert_eq!(
        signal(body, WireFormat::Protobuf, "/"),
        Some(Signal::Metrics)
    );
}

#[test]
fn every_signal_is_inferred_and_keeps_its_explicit_path() {
    let logs = ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            scope_logs: vec![ScopeLogs {
                log_records: vec![LogRecord {
                    time_unix_nano: 42,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec();
    let metrics = ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            scope_metrics: vec![ScopeMetrics {
                metrics: vec![Metric {
                    name: "test.counter".into(),
                    data: Some(Data::Sum(Sum::default())),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec();
    let traces = ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            scope_spans: vec![ScopeSpans {
                spans: vec![Span {
                    name: "test.span".into(),
                    trace_id: vec![1; 16],
                    span_id: vec![2; 8],
                    start_time_unix_nano: 1,
                    end_time_unix_nano: 2,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec();
    for (expected, body) in [
        (Signal::Logs, logs),
        (Signal::Metrics, metrics),
        (Signal::Traces, traces),
    ] {
        for path in ["/", expected.path()] {
            assert_eq!(signal(&body, WireFormat::Protobuf, path), Some(expected));
        }
        let json = serde_json::json!({expected.json_key(): []}).to_string();
        assert_eq!(
            signal(json.as_bytes(), WireFormat::Json, "/"),
            Some(expected)
        );
        assert_eq!(
            signal(json.as_bytes(), WireFormat::Json, expected.path()),
            Some(expected)
        );
    }
}

#[test]
fn unknown_or_mixed_signals_are_not_silently_logs() {
    for body in [b"".as_slice(), b"not protobuf"] {
        assert_eq!(signal(body, WireFormat::Protobuf, "/"), None);
    }
    for body in [
        r#"{}"#,
        r#"{"resourceProfiles":[]}"#,
        r#"{"resourceLogs":[],"resourceMetrics":[]}"#,
    ] {
        assert_eq!(signal(body.as_bytes(), WireFormat::Json, "/"), None);
    }
    assert_eq!(
        signal(b"", WireFormat::Protobuf, "/v1/traces"),
        Some(Signal::Traces)
    );
    assert_eq!(
        signal(br#"{"resourceMetrics":[]}"#, WireFormat::Json, "/v1/logs"),
        None
    );
    let metric = b"\x0a\x09\x12\x07\x12\x05\x0a\x01m\x3a\x00";
    assert_eq!(signal(metric, WireFormat::Protobuf, "/v1/logs"), None);
}
