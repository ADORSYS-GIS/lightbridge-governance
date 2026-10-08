//! Traces counterpart of the parent module's logs stamping and `metrics.rs`'s
//! metrics stamping -- lightbridge-governance#369.
//!
//! Before this, traces were forwarded unstamped, so the public collector's
//! static `X-Source` default (`claude-code`) was the only source they ever
//! carried. VS Code Copilot Chat is, in practice, the one local tool that
//! exports traces (Claude Code and Codex are configured for logs and metrics
//! only), so its spans landed in lightbridge-authz as `claude-code`
//! executions -- 528,244 rows in 30 days (lightbridge-authz#769).
//!
//! ## The signal: the resource's own `service.name`, then span attribute keys
//!
//! Unlike metrics, `service.name` is the PRIMARY signal here, and that is a
//! deliberate departure from `metrics.rs`, which ignores it. A real Copilot
//! Chat 0.68.0 trace (captured 2026-10-08) carries `service.name =
//! copilot-chat` on every resource, but only SOME of its spans carry a
//! `copilot_chat.`-prefixed attribute: `invoke_agent`, `chat` and
//! `execute_tool` do; `embeddings` and the `vscode.chat.*` timing spans do
//! not. A batch holding only the latter would go unstamped -- and back to
//! `claude-code` -- if span attributes were the only signal. Trusting
//! `service.name` is the same bounded trust this module already extends to
//! `event.name` (ADR-0016: a local process can already forge this
//! developer's telemetry), not a wider one.
//!
//! The span-attribute prefix stays as a second signal, so a Copilot build
//! that renames its service is still recognised by its own attribute
//! namespace. Neither signal present means no stamp -- never a guess.

use opentelemetry_proto::tonic::{
    collector::trace::v1::ExportTraceServiceRequest,
    common::v1::{AnyValue, KeyValue, any_value::Value},
    resource::v1::Resource,
    trace::v1::ResourceSpans,
};
use prost::Message;

use super::SOURCE_ATTRIBUTE;
use crate::otel_daemon::receive::WireFormat;

/// Copilot Chat's own `service.name`, as captured.
const COPILOT_CHAT_SERVICE_NAME: &str = "copilot-chat";

/// Copilot Chat's own span-attribute namespace -- the same prefix `metrics.rs`
/// keys metric names on.
const COPILOT_CHAT_ATTRIBUTE_PREFIX: &str = "copilot_chat.";

/// Same decode/mutate/re-encode shape, and the same schema-loss trade-off, as
/// the parent module's `enrich` and `metrics.rs`'s `enrich_metrics`.
pub(in crate::otel_daemon) fn enrich_traces(body: &[u8], format: WireFormat) -> Vec<u8> {
    let parsed: Option<ExportTraceServiceRequest> = match format {
        WireFormat::Json => serde_json::from_slice(body).ok(),
        WireFormat::Protobuf => ExportTraceServiceRequest::decode(body).ok(),
    };
    let Some(mut request) = parsed else {
        return body.to_vec();
    };

    let mut changed = false;
    for resource_spans in &mut request.resource_spans {
        let Some(source) = resource_source(resource_spans) else {
            continue;
        };
        let resource = resource_spans
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

/// The canonical source one resource group identifies, if any.
fn resource_source(resource_spans: &ResourceSpans) -> Option<&'static str> {
    let named_copilot = resource_spans
        .resource
        .as_ref()
        .is_some_and(|resource| service_name(resource) == Some(COPILOT_CHAT_SERVICE_NAME));
    let carries_copilot_attributes = resource_spans
        .scope_spans
        .iter()
        .flat_map(|scope| &scope.spans)
        .flat_map(|span| &span.attributes)
        .any(|attribute| attribute.key.starts_with(COPILOT_CHAT_ATTRIBUTE_PREFIX));
    (named_copilot || carries_copilot_attributes).then_some("github-copilot")
}

/// The resource's `service.name`, when it is exactly one string. A duplicated
/// key is ambiguous, not a first-match win -- the same rule `event_source`
/// applies to `event.name`.
fn service_name(resource: &Resource) -> Option<&str> {
    let mut matches = resource
        .attributes
        .iter()
        .filter(|a| a.key == "service.name");
    let value = matches.next()?.value.as_ref()?.value.as_ref()?;
    if matches.next().is_some() {
        return None;
    }
    match value {
        Value::StringValue(name) => Some(name),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
