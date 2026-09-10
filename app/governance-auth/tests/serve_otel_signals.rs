//! Every supported OTLP signal traverses the real daemon and durable drain.
mod support;

use anyhow::Result;
use opentelemetry_proto::tonic::{
    collector::trace::v1::ExportTraceServiceRequest,
    trace::v1::{ResourceSpans, ScopeSpans, Span},
};
use prost::Message;
use support::{
    copilot as fixture, harness::Harness, interrupt, raw_collector::RawCollector,
    serve_otel::Daemon,
};

#[tokio::test]
async fn root_binary_metrics_and_traces_reach_their_own_endpoints() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let collector = RawCollector::start().await?;
    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;
    let metrics = b"\x0a\x09\x12\x07\x12\x05\x0a\x01m\x3a\x00".to_vec();
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
    for (path, bytes) in [("/v1/metrics", metrics), ("/v1/traces", traces)] {
        let before = collector.requests()?.len();
        let response = daemon
            .post_bytes_response("/", "application/x-protobuf", bytes.clone())
            .await?;
        assert_eq!(response.status().as_u16(), 200);
        interrupt::until("a binary signal to be forwarded", || {
            Ok(collector.requests()?.len() > before)
        })
        .await?;
        let requests = collector.requests()?;
        let (actual, content_type, body) = requests.last().expect("forwarded");
        assert_eq!(actual, path);
        assert_eq!(content_type, "application/x-protobuf");
        assert_eq!(body, &bytes, "do not re-encode protobuf");
    }
    for (path, key) in [
        ("/v1/logs", "resourceLogs"),
        ("/v1/metrics", "resourceMetrics"),
        ("/v1/traces", "resourceSpans"),
    ] {
        let before = collector.requests()?.len();
        let bytes = serde_json::json!({key: [{"resource": {"attributes": []}}]})
            .to_string()
            .into_bytes();
        let response = daemon
            .post_bytes_response(path, "application/json", bytes)
            .await?;
        assert_eq!(response.status().as_u16(), 200);
        interrupt::until("a JSON signal to be forwarded", || {
            Ok(collector.requests()?.len() > before)
        })
        .await?;
        let requests = collector.requests()?;
        let (actual, content_type, _) = requests.last().expect("forwarded");
        assert_eq!(actual, path);
        assert_eq!(
            content_type,
            "application/json",
            "expected {path}, received metadata: {:?}",
            requests
                .iter()
                .map(|(p, c, b)| (p, c, b.len()))
                .collect::<Vec<_>>()
        );
    }
    let response = daemon
        .post_bytes_response("/", "application/x-protobuf", vec![])
        .await?;
    assert_eq!(
        response.status().as_u16(),
        400,
        "ambiguous signal is refused"
    );
    daemon.stop()?;
    Ok(())
}
