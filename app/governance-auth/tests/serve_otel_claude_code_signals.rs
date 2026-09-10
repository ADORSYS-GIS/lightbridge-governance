//! Claude Code's shared-endpoint + protobuf contract, driven against the real
//! daemon -- the closest automated substitute for the live Claude Code replay
//! the `otel-daemon-misrouted-signals` runbook says was never performed for
//! this incident.
//!
//! Codex's failure mode was a bare root URL sitting in a signal-specific
//! config field, which the (pre-fix) daemon defaulted to `/v1/logs`
//! regardless of the real signal. Claude Code's own writer
//! (`configure_claude_code`) never produces that shape: it writes one shared
//! `OTEL_EXPORTER_OTLP_ENDPOINT` plus `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf`,
//! and Claude Code's OTEL SDK appends `/v1/logs` / `/v1/metrics` to that base
//! itself before sending -- see the runbook's "Claude Code" section. This test
//! builds protobuf export bodies shaped like a real Claude Code export
//! (`service.name=claude-code` resource attributes, a `claude_code.*` metric)
//! and posts them to exactly that endpoint contract: the daemon's own fixed
//! loopback address with each signal's own path.
//!
//! What this proves: the daemon admits and routes a Claude-Code-shaped
//! protobuf export by its own signal, never relabeling one onto another the
//! way the Codex root-URL bug did. What this does NOT prove: it does not run
//! the actual Claude Code binary, its OTEL SDK, or exercise managed-settings
//! layering -- a genuine live-client replay is still open, exactly as the
//! runbook records.
mod support;

use anyhow::Result;
use opentelemetry_proto::tonic::{
    collector::{logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest},
    common::v1::{AnyValue, KeyValue, any_value::Value as AnyValueInner},
    logs::v1::{LogRecord, ResourceLogs, ScopeLogs},
    metrics::v1::{Metric, ResourceMetrics, ScopeMetrics, Sum, metric::Data},
    resource::v1::Resource,
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
            value: Some(AnyValueInner::StringValue(value.to_owned())),
        }),
        ..Default::default()
    }
}

/// The resource shape Claude Code's `OTEL_RESOURCE_ATTRIBUTES` produces --
/// see `resource_attributes_value` and `claude_code_settings_still_carry_the_whole_telemetry_set`.
fn claude_code_resource() -> Resource {
    Resource {
        attributes: vec![
            string_attr("service.name", "claude-code"),
            string_attr("user.id", "abc-123"),
        ],
        ..Default::default()
    }
}

#[tokio::test]
async fn claude_code_shaped_logs_and_metrics_reach_their_own_endpoint() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let collector = RawCollector::start().await?;
    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;

    let logs = ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(claude_code_resource()),
            scope_logs: vec![ScopeLogs {
                log_records: vec![LogRecord {
                    time_unix_nano: 1_788_191_912_613_000_000,
                    body: Some(AnyValue {
                        value: Some(AnyValueInner::StringValue("tool_result".to_owned())),
                    }),
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
            resource: Some(claude_code_resource()),
            scope_metrics: vec![ScopeMetrics {
                metrics: vec![Metric {
                    name: "claude_code.token.usage".into(),
                    data: Some(Data::Sum(Sum::default())),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec();

    for (path, body) in [("/v1/logs", logs), ("/v1/metrics", metrics)] {
        let before = collector.requests()?.len();
        let response = daemon
            .post_bytes_response(path, "application/x-protobuf", body.clone())
            .await?;
        assert_eq!(
            response.status().as_u16(),
            200,
            "a Claude-Code-shaped {path} export must be admitted, not refused as ambiguous"
        );
        interrupt::until("the Claude-Code-shaped export to be forwarded", || {
            Ok(collector.requests()?.len() > before)
        })
        .await?;
        let requests = collector.requests()?;
        let (actual_path, content_type, actual_body) = requests.last().expect("forwarded");
        assert_eq!(
            actual_path, path,
            "must route by its own signal -- never relabeled the way Codex's \
             root-URL metrics export was relabeled onto /v1/logs"
        );
        assert_eq!(
            content_type, "application/x-protobuf",
            "protocol preserved as http/protobuf, Claude Code's configured protocol"
        );
        assert_eq!(actual_body, &body, "forwarded verbatim, not re-encoded");
    }

    daemon.stop()?;
    Ok(())
}
