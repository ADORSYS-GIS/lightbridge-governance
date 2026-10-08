//! lightbridge-governance#369, driven against the real daemon: a Copilot Chat
//! trace export posted to the loopback `/v1/traces` reaches the collector
//! carrying `governance.source = github-copilot`.
//!
//! The unit tests in `otel_daemon/source_stamp/traces/tests.rs` prove
//! `enrich_traces` itself; this proves the daemon actually CALLS it on the
//! traces signal. Before #369 `request.rs` passed traces through untouched, and
//! a regression back to that would leave every unit test green.

mod support;

use anyhow::Result;
use opentelemetry_proto::tonic::{
    collector::trace::v1::ExportTraceServiceRequest,
    common::v1::{AnyValue, KeyValue, any_value::Value},
    resource::v1::Resource,
    trace::v1::{ResourceSpans, ScopeSpans, Span},
};
use prost::Message;
use support::{
    copilot as fixture, harness::Harness, interrupt, raw_collector::RawCollector,
    serve_otel::Daemon,
};

fn string_attr(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_owned(),
        value: Some(AnyValue {
            value: Some(Value::StringValue(value.to_owned())),
        }),
        ..Default::default()
    }
}

/// One `execute_tool` span under Copilot Chat's resource -- names and keys from
/// the 2026-10-08 capture, values synthetic.
fn copilot_trace() -> Vec<u8> {
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource {
                attributes: vec![string_attr("service.name", "copilot-chat")],
                ..Default::default()
            }),
            scope_spans: vec![ScopeSpans {
                spans: vec![Span {
                    trace_id: vec![1; 16],
                    span_id: vec![2; 8],
                    name: "execute_tool list_dir".to_owned(),
                    start_time_unix_nano: 1_788_191_912_613_000_000,
                    end_time_unix_nano: 1_788_191_912_674_000_000,
                    attributes: vec![string_attr("gen_ai.tool.name", "list_dir")],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

#[tokio::test]
async fn a_copilot_trace_is_forwarded_stamped_github_copilot() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let collector = RawCollector::start().await?;
    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;

    let response = daemon
        .post_bytes_response("/v1/traces", "application/x-protobuf", copilot_trace())
        .await?;
    assert_eq!(response.status().as_u16(), 200);
    interrupt::until("the Copilot trace to be forwarded", || {
        Ok(!collector.requests()?.is_empty())
    })
    .await?;

    let requests = collector.requests()?;
    let (path, _, body) = requests.last().expect("forwarded");
    assert_eq!(path, "/v1/traces", "routed by its own signal");
    let forwarded = ExportTraceServiceRequest::decode(body.as_slice())?;
    let sources: Vec<&str> = forwarded.resource_spans[0]
        .resource
        .as_ref()
        .map(|resource| {
            resource
                .attributes
                .iter()
                .filter(|a| a.key == "governance.source")
                .filter_map(|a| match a.value.as_ref()?.value.as_ref()? {
                    Value::StringValue(v) => Some(v.as_str()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        sources,
        vec!["github-copilot"],
        "exactly one governance.source, and it names Copilot -- not the collector's claude-code default"
    );

    daemon.stop()?;
    Ok(())
}
