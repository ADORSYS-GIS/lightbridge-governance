//! Infer a bare-URL signal using official OTLP decoders, never re-encode it.
//!
//! OTLP envelopes share field numbers, so successful decoding alone is not
//! evidence. Require recognizable records and exactly one matching signal.
//! Empty/ambiguous exports need an explicit signal URL.

use opentelemetry_proto::tonic::collector::{
    logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest,
    trace::v1::ExportTraceServiceRequest,
};
use prost::Message;

use super::signal::Signal;

pub fn signal(body: &[u8]) -> Option<Signal> {
    let logs = ExportLogsServiceRequest::decode(body).is_ok_and(|request| {
        request
            .resource_logs
            .iter()
            .flat_map(|r| &r.scope_logs)
            .flat_map(|s| &s.log_records)
            .any(|r| r.time_unix_nano != 0 || r.observed_time_unix_nano != 0 || r.body.is_some())
    });
    let metrics = ExportMetricsServiceRequest::decode(body).is_ok_and(|request| {
        request
            .resource_metrics
            .iter()
            .flat_map(|r| &r.scope_metrics)
            .flat_map(|s| &s.metrics)
            .any(|m| !m.name.is_empty() && m.data.is_some())
    });
    let traces = ExportTraceServiceRequest::decode(body).is_ok_and(|request| {
        request
            .resource_spans
            .iter()
            .flat_map(|r| &r.scope_spans)
            .flat_map(|s| &s.spans)
            .any(|s| !s.name.is_empty() && !s.trace_id.is_empty())
    });
    match (logs, metrics, traces) {
        (true, false, false) => Some(Signal::Logs),
        (false, true, false) => Some(Signal::Metrics),
        (false, false, true) => Some(Signal::Traces),
        _ => None,
    }
}
