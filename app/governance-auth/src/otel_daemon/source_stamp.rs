//! Stamps a trusted `governance.source` resource attribute derived from each
//! resourceLogs group's own event names -- governance#358.
//!
//! ## Why this belongs here, not at the public collector
//!
//! lightbridge-authz ADR-0028 D8 rules out deriving the usage-store `source`
//! from `event.name`/`service.name` at the PUBLIC collector: any internet
//! caller holding a valid-but-generic credential could claim to be any tool.
//! That threat model does not transfer to this loopback listener. ADR-0016
//! already accepts that any local process can forge telemetry attributable
//! to this developer ("the residual risk is accepted: forged telemetry from
//! a machine the developer already controls"), so deriving a same-developer
//! TOOL label from a signal a local process already legitimately controls
//! adds no new exposure over what that ADR already accepts. `codex_cost::
//! enrich` already relies on the identical event-name signal
//! (`codex.sse_event`) for cost estimation, at this same trust boundary and
//! admission point.
//!
//! ⚠️ The public collector's `resource` processor (`charts/lightbridge-
//! governance` `_helpers.tpl`) does NOT overwrite this key once set --
//! `insert`, not `upsert` (governance#358), specifically so this stamp
//! survives. That means a `manual`-profile client calling the collector
//! directly, under the same shared audience credential this loopback
//! listener's daemon also forwards under, can set its own `governance.source`
//! and have it preserved too. Accepted as a bounded residual risk (found in
//! PR review on #359, not fixed before merge) -- see the chart helper's own
//! comment for the full reasoning and why it is bounded.
//!
//! ## Event-name taxonomy
//!
//! Verified against `docs/rfc/sources/claude-codex-usage-investigation.md`
//! (itself checked against Anthropic's own monitoring-usage docs and
//! `codex-rs/otel`): Codex's own event names are ALL namespaced under
//! `codex.` (`codex.api_request`, `codex.sse_event`, `codex.tool_result`,
//! ...). Claude Code's are bare (`api_request`, `user_prompt`, `tool_result`,
//! `auth`, `plugin_*`, ...). The two sets do not collide: `codex.` never
//! prefixes a Claude Code event, and none of Claude Code's own names carry
//! any prefix at all.
//!
//! An event name matching neither taxonomy leaves `governance.source`
//! untouched for that resource -- absence is the honest answer, never a
//! guess.
//!
//! ## VS Code Copilot Chat is on the METRICS signal, not logs
//!
//! Found live 2026-09-23: real Copilot Chat sessions land on this daemon
//! (confirmed with matching `user_name`/timing), but the rich data --
//! `gen_ai.client.token.usage`, `gen_ai.client.operation.duration`,
//! `copilot_chat.time_to_first_token`, `copilot_chat.tool.call.count`,
//! `copilot_chat.agent.turn.count`, etc -- is exported as METRICS, not logs.
//! `event.name`-based stamping above cannot reach it: metrics carry no
//! `event.name` attribute at all, they carry a metric NAME. Every observed
//! Copilot metrics batch carries at least one `copilot_chat.`-prefixed
//! metric name alongside the shared `gen_ai.client.*` ones (VS Code's OTel
//! SDK batches all registered instruments together each collection tick),
//! so keying on that one prefix reliably labels the whole resource --
//! [`metrics::enrich_metrics`], re-exported below and called from
//! `request.rs` on `Signal::Metrics`, parallel to this module's own
//! logs-only [`enrich`]. Split into its own file by the LoC gate
//! (lightbridge-governance#172).
//!
//! Before this, an unrecognised resource on the `aiCliOtel` collector fell
//! back to that collector's own `X-Source` default (hardcoded to
//! `"claude-code"`) -- so every Copilot Chat metric was silently counted as
//! Claude Code usage. `KNOWN_SOURCES` (lightbridge-authz ADR-0028 D4)
//! already reserves `"github-copilot"` for exactly this tool; this module
//! now actually stamps it instead of leaving the gap for the collector's
//! coarse default to paper over.
//!
//! ## What this does NOT do
//!
//! This does not touch identity (`user.id`/`account_id`/...) -- that is
//! `normalize::stamp`'s job, at drain/forward time, from the bearer's own
//! claims. Source and identity are independent labels stamped by different
//! modules for different reasons; this one runs at admission (like
//! `codex_cost::enrich`, and for the same reason -- no bearer is needed, so
//! there is nothing to gain by waiting for forward time) and needs no token.

use opentelemetry_proto::tonic::{
    collector::logs::v1::ExportLogsServiceRequest,
    common::v1::{AnyValue, KeyValue, any_value::Value},
    logs::v1::LogRecord,
    resource::v1::Resource,
};
use prost::Message;

use super::receive::WireFormat;

mod metrics;
pub(super) use metrics::enrich_metrics;

/// The resource attribute this module writes. Matches the key
/// `charts/lightbridge-governance`'s `publicOtelCollector` helper stamps --
/// see governance#358's acceptance record for the corresponding chart-side
/// change required so this value survives instead of being overwritten.
const SOURCE_ATTRIBUTE: &str = "governance.source";

/// Claude Code's own bare (unnamespaced) event names -- see the module doc.
/// `plugin_` is a documented prefix (`plugin_*`); the rest are exact names.
const CLAUDE_CODE_EVENTS: [&str; 9] = [
    "user_prompt",
    "assistant_response",
    "api_request",
    "api_error",
    "api_refusal",
    "tool_decision",
    "tool_result",
    "auth",
    "mcp_server_connection",
];
const CLAUDE_CODE_EVENT_PREFIX: &str = "plugin_";
const CODEX_EVENT_PREFIX: &str = "codex.";

/// Decodes the whole request through the compiled OTLP types to stamp one
/// attribute, then re-encodes it (found in PR review on #359) -- any field
/// unknown to this pinned `opentelemetry-proto`/serde schema is silently
/// dropped from every `resource_logs` entry in the batch, not just the one
/// that matched, on both the protobuf and JSON paths. Not a new risk: the
/// K8s collector's own processor pipeline already round-trips every payload
/// through its own typed model, and `codex_cost::enrich` already does the
/// identical decode/mutate/re-encode at this same admission point. Accepted
/// as the standing cost of touching OTLP via typed structs at all; only
/// bytes this function actually changes (`changed == true` below) pay it.
pub(super) fn enrich(body: &[u8], format: WireFormat) -> Vec<u8> {
    let parsed: Option<ExportLogsServiceRequest> = match format {
        WireFormat::Json => serde_json::from_slice(body).ok(),
        WireFormat::Protobuf => ExportLogsServiceRequest::decode(body).ok(),
    };
    let Some(mut request) = parsed else {
        return body.to_vec();
    };

    let mut changed = false;
    for resource_logs in &mut request.resource_logs {
        let Some(source) = resource_logs
            .scope_logs
            .iter()
            .flat_map(|scope| &scope.log_records)
            .find_map(event_source)
        else {
            continue;
        };
        let resource = resource_logs.resource.get_or_insert_with(Resource::default);
        // Strip any client-supplied value first, then insert ours
        // unconditionally -- the value this module derives is more reliable
        // than whatever a client already put there, so it replaces it rather
        // than layering on top (same "strip then set" shape `normalize`
        // uses for identity, for the same reason).
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

/// The canonical source a single log record's `event.name` identifies, if
/// any. `None` covers both "no `event.name` attribute" and "a name neither
/// taxonomy claims" -- both leave `governance.source` untouched for this
/// resource rather than guessing.
fn event_source(record: &LogRecord) -> Option<&'static str> {
    let mut matches = record.attributes.iter().filter(|a| a.key == "event.name");
    let value = matches.next()?.value.as_ref()?.value.as_ref()?;
    // More than one `event.name` on the same record is ambiguous, not a
    // first-match win -- same rule `codex_cost::string` already applies to
    // every attribute it reads.
    if matches.next().is_some() {
        return None;
    }
    let Value::StringValue(name) = value else {
        return None;
    };
    if name.starts_with(CODEX_EVENT_PREFIX) {
        Some("codex")
    } else if CLAUDE_CODE_EVENTS.contains(&name.as_str())
        || name.starts_with(CLAUDE_CODE_EVENT_PREFIX)
    {
        Some("claude-code")
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
