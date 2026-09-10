//! Exercise the production classifier and sender on an ephemeral collector port.
//! Unlike a fixed-port daemon test, live IDE exports cannot enter this receiver.

#[path = "raw_collector.rs"]
mod raw_collector;

use anyhow::Result;
use opentelemetry_proto::tonic::{
    collector::trace::v1::ExportTraceServiceRequest,
    trace::v1::{ResourceSpans, ScopeSpans, Span},
};
use prost::Message;

use crate::{
    config::OauthConfig,
    otel_daemon::{classify, receive::WireFormat},
    redacted::Redacted,
};

#[tokio::test]
async fn mixed_signal_formats_keep_their_bytes_and_destination() -> Result<()> {
    let collector = raw_collector::RawCollector::start().await?;
    let config = OauthConfig {
        issuer: "https://unused.invalid".into(),
        client_id: "test".into(),
        scopes: String::new(),
        audience: None,
        otel_endpoint: Some(collector.base_url.clone()),
        otel_token: None,
        gateway_url: None,
        profile: Default::default(),
        profile_explicit: None,
        copilot_spool_path: None,
        otel_headers_debounce_ms: 240000,
        open_browser: false,
        token_exchange: None,
    };
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
    let metrics = b"\x0a\x09\x12\x07\x12\x05\x0a\x01m\x3a\x00".to_vec();
    let inputs = [
        ("/v1/metrics", WireFormat::Protobuf, metrics),
        ("/v1/traces", WireFormat::Protobuf, traces),
        (
            "/v1/logs",
            WireFormat::Json,
            br#"{"resourceLogs":[]}"#.to_vec(),
        ),
        (
            "/v1/metrics",
            WireFormat::Json,
            br#"{"resourceMetrics":[]}"#.to_vec(),
        ),
        (
            "/v1/traces",
            WireFormat::Json,
            br#"{"resourceSpans":[]}"#.to_vec(),
        ),
    ];
    let http = reqwest::Client::new();
    // Repeated transitions on one HTTP connection exercise format handling,
    // asserting every request rather than retrying a failed assertion.
    for _ in 0..10 {
        for (path, format, body) in &inputs {
            let signal = classify::signal(body, *format, "/").expect("known signal");
            let verdict = super::post(
                &http,
                &config,
                &Redacted::new("fixture".to_owned()),
                signal,
                body,
                *format == WireFormat::Json,
            )
            .await?;
            assert_eq!(verdict, super::Verdict::Accepted);
            let requests = collector.requests()?;
            let (actual, content_type, bytes) = requests.last().expect("received");
            assert_eq!(actual, path);
            assert_eq!(content_type, format.content_type());
            assert_eq!(bytes, body);
        }
    }
    assert_eq!(collector.requests()?.len(), inputs.len() * 10);
    Ok(())
}
