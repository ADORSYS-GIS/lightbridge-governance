//! Metrics counterpart of the parent module's logs-only stamping -- split out
//! by the LoC gate (lightbridge-governance#172). See `source_stamp`'s own
//! module doc ("VS Code Copilot Chat is on the METRICS signal, not logs") for
//! why this needs its own signal-specific check rather than folding into the
//! logs path: metrics carry no `event.name` attribute, they carry a metric
//! NAME, and Copilot Chat's rich telemetry (`gen_ai.client.*`,
//! `copilot_chat.*`) is exported as metrics, not logs.

use opentelemetry_proto::tonic::{
    collector::metrics::v1::ExportMetricsServiceRequest,
    common::v1::{AnyValue, KeyValue, any_value::Value},
    metrics::v1::Metric,
    resource::v1::Resource,
};
use prost::Message;

use super::SOURCE_ATTRIBUTE;
use crate::otel_daemon::receive::WireFormat;

/// VS Code Copilot Chat's own metric namespace -- see the parent module's doc
/// for why metrics, not `event.name`, are the taxonomy signal for this
/// source.
const COPILOT_CHAT_METRIC_PREFIX: &str = "copilot_chat.";

/// Same decode/mutate/re-encode shape as the parent module's `enrich`, and
/// the identical schema-loss trade-off it documents; only the request type
/// and per-item taxonomy check differ.
///
/// `pub(in crate::otel_daemon)`, not `pub(super)`: `request.rs` (this
/// module's grandparent's sibling) calls this via `source_stamp`'s
/// re-export, so it needs to be visible two levels up, not just one.
pub(in crate::otel_daemon) fn enrich_metrics(body: &[u8], format: WireFormat) -> Vec<u8> {
    let parsed: Option<ExportMetricsServiceRequest> = match format {
        WireFormat::Json => serde_json::from_slice(body).ok(),
        WireFormat::Protobuf => ExportMetricsServiceRequest::decode(body).ok(),
    };
    let Some(mut request) = parsed else {
        return body.to_vec();
    };

    let mut changed = false;
    for resource_metrics in &mut request.resource_metrics {
        let Some(source) = resource_metrics
            .scope_metrics
            .iter()
            .flat_map(|scope| &scope.metrics)
            .find_map(metric_source)
        else {
            continue;
        };
        let resource = resource_metrics
            .resource
            .get_or_insert_with(Resource::default);
        resource.attributes.retain(|a| a.key != SOURCE_ATTRIBUTE);
        resource.attributes.push(KeyValue {
            key: SOURCE_ATTRIBUTE.to_owned(),
            value: Some(AnyValue {
                value: Some(Value::StringValue(source.to_owned())),
            }),
            ..Default::default()
        });
        changed = true;
    }

    if !changed {
        return body.to_vec();
    }
    match format {
        WireFormat::Json => serde_json::to_vec(&request).unwrap_or_else(|_| body.to_vec()),
        WireFormat::Protobuf => request.encode_to_vec(),
    }
}

/// The canonical source one metric's NAME identifies, if any. Unlike the
/// parent module's `event_source`, this checks a metric name prefix, not an
/// attribute -- Copilot Chat's OTel SDK batches every registered instrument
/// together each collection tick, so any one `copilot_chat.`-prefixed metric
/// in a resource is enough to label the whole resource.
fn metric_source(metric: &Metric) -> Option<&'static str> {
    if metric.name.starts_with(COPILOT_CHAT_METRIC_PREFIX) {
        Some("github-copilot")
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
