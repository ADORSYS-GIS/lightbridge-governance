//! Microsoft Foundry normalizer (RFC-0002).
//!
//! Foundry hosted agents export OTLP to our public endpoint. This normalizer
//! extracts the relevant fields from Foundry's OTLP spans and maps them to
//! the unified model. Attributes arrive as real OTLP proto3 JSON (attribute
//! arrays, string encoded ints).

use chrono::{DateTime, Utc};
use governance_core::ingest::{ExecutionInput, ModelCallInput, ToolCallInput};

use super::{
    Normalizer, NormalizerError, TelemetryPayload,
    otlp::{attr_i64, attr_string, duration_ms, events, span_i64, span_string},
};

pub struct FoundryNormalizer;

impl Normalizer for FoundryNormalizer {
    fn normalize(&self, payload: &serde_json::Value) -> Result<TelemetryPayload, NormalizerError> {
        let resource_spans = payload
            .get("resourceSpans")
            .and_then(|v| v.as_array())
            .ok_or_else(|| NormalizerError::MissingField("resourceSpans".to_owned()))?;

        let mut executions = Vec::new();

        for resource_span in resource_spans {
            let resource = resource_span
                .get("resource")
                .ok_or_else(|| NormalizerError::MissingField("resource".to_owned()))?;

            let user_email = attr_string(resource, "user.email", "resource")?;

            let scope_spans = resource_span
                .get("scopeSpans")
                .and_then(|v| v.as_array())
                .ok_or_else(|| NormalizerError::MissingField("scopeSpans".to_owned()))?;

            for scope_span in scope_spans {
                let spans = scope_span
                    .get("spans")
                    .and_then(|v| v.as_array())
                    .ok_or_else(|| NormalizerError::MissingField("spans".to_owned()))?;

                for span in spans {
                    let execution = normalize_span(span, user_email.as_deref())?;
                    executions.push(execution);
                }
            }
        }

        Ok(TelemetryPayload { executions })
    }
}

fn normalize_span(
    span: &serde_json::Value,
    user_email: Option<&str>,
) -> Result<ExecutionInput, NormalizerError> {
    let trace_id = span_string(span, "traceId")?
        .ok_or_else(|| NormalizerError::MissingField("traceId".to_owned()))?;
    let span_id = span_string(span, "spanId")?
        .ok_or_else(|| NormalizerError::MissingField("spanId".to_owned()))?;

    // `session.id` is intentionally not persisted (no execution-grouping
    // feature uses it yet). Reading it here only rejects a *structurally*
    // invalid attribute entry (e.g. a `value` that isn't even an object) --
    // `attr_string` treats a present-but-wrong-kind value (say,
    // `{"intValue": "42"}` where `{"stringValue": ...}` was expected) as
    // `Ok(None)`, indistinguishable from absent. That is deliberate, general
    // behavior of the helper (see `wrong_value_kind_is_none_not_an_error` in
    // otlp.rs), not a hard rejection specific to this field.
    attr_string(span, "session.id", "span")?;
    let model_name = attr_string(span, "model.name", "span")?
        .ok_or_else(|| NormalizerError::MissingField("model.name".to_owned()))?;
    // Token counts are optional: a span that omits them yields a model call
    // with unknown cost (story #31 AC6), not a rejection.
    let input_tokens = attr_i64(span, "tokens.input", "span")?;
    let output_tokens = attr_i64(span, "tokens.output", "span")?;

    let start_time_unix_nano = span_i64(span, "startTimeUnixNano")?;
    let end_time_unix_nano = span_i64(span, "endTimeUnixNano")?;

    let started_at = DateTime::<Utc>::from_timestamp_nanos(start_time_unix_nano);
    let duration_ms = duration_ms(start_time_unix_nano, end_time_unix_nano)?;

    // The model call and each tool call need their own (trace_id, span_id) --
    // the idempotency key is unique per row. Child ids are derived from the
    // parent span id so they stay deterministic across reprocessing.
    let model_call_span_id = format!("{span_id}:mc");
    let model_call = ModelCallInput {
        trace_id: trace_id.clone(),
        span_id: model_call_span_id,
        model: model_name,
        input_tokens,
        output_tokens,
    };

    let mut tool_calls = Vec::new();

    for (idx, event) in events(span, "span")?.iter().enumerate() {
        let event_name = span_string(event, "name")?;
        if event_name.as_deref() == Some("tool.call") {
            let tool_name = attr_string(event, "tool.name", "event")?
                .ok_or_else(|| NormalizerError::MissingField("tool.name".to_owned()))?;
            let tool_duration_ms = attr_i64(event, "duration.ms", "event")?
                .ok_or_else(|| NormalizerError::MissingField("duration.ms".to_owned()))?;

            tool_calls.push(ToolCallInput {
                trace_id: trace_id.clone(),
                span_id: format!("{span_id}:tc:{idx}"),
                tool_name,
                duration_ms: tool_duration_ms,
            });
        }
    }

    Ok(ExecutionInput {
        trace_id,
        span_id,
        user_email: user_email.map(|s| s.to_owned()),
        started_at,
        duration_ms,
        model_calls: vec![model_call],
        tool_calls,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A span in the real OTLP proto3 JSON shape (attribute arrays, string
    /// encoded ints), including a `tool.call` event so the tool-call
    /// extraction path is exercised by default.
    fn valid_payload() -> serde_json::Value {
        json!({
            "resourceSpans": [{
                "resource": {
                    "attributes": [
                        { "key": "user.email", "value": { "stringValue": "user@example.com" } }
                    ]
                },
                "scopeSpans": [{
                    "spans": [{
                        "traceId": "trace-123",
                        "spanId": "span-456",
                        "startTimeUnixNano": "1700000000000000000",
                        "endTimeUnixNano": "1700000005000000000",
                        "attributes": [
                            { "key": "session.id", "value": { "stringValue": "session-789" } },
                            { "key": "model.name", "value": { "stringValue": "gpt-4" } },
                            { "key": "tokens.input", "value": { "intValue": "1000" } },
                            { "key": "tokens.output", "value": { "intValue": "500" } }
                        ],
                        "events": [{
                            "name": "tool.call",
                            "attributes": [
                                { "key": "tool.name", "value": { "stringValue": "bash" } },
                                { "key": "duration.ms", "value": { "intValue": "1500" } }
                            ]
                        }]
                    }]
                }]
            }]
        })
    }

    #[test]
    fn normalizes_valid_foundry_payload() {
        let normalizer = FoundryNormalizer;
        let result = normalizer.normalize(&valid_payload()).expect("normalize");

        assert_eq!(result.executions.len(), 1);
        let exec = &result.executions[0];
        assert_eq!(exec.trace_id, "trace-123");
        assert_eq!(exec.span_id, "span-456");
        assert_eq!(exec.user_email, Some("user@example.com".to_owned()));
        assert_eq!(exec.duration_ms, 5000);
        assert_eq!(exec.model_calls.len(), 1);
        assert_eq!(exec.model_calls[0].model, "gpt-4");
        assert_eq!(exec.model_calls[0].input_tokens, Some(1000));
        assert_eq!(exec.model_calls[0].output_tokens, Some(500));
        assert_eq!(exec.tool_calls.len(), 1);
        assert_eq!(exec.tool_calls[0].tool_name, "bash");
        assert_eq!(exec.tool_calls[0].duration_ms, 1500);
    }

    #[test]
    fn rejects_missing_resource_spans() {
        let normalizer = FoundryNormalizer;
        let result = normalizer.normalize(&json!({}));
        assert!(matches!(result, Err(NormalizerError::MissingField(_))));
    }

    #[test]
    fn rejects_missing_model_name() {
        let mut payload = valid_payload();
        let attributes = payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"]
            .as_array_mut()
            .expect("attributes array");
        attributes.retain(|a| a.get("key").and_then(|k| k.as_str()) != Some("model.name"));

        let normalizer = FoundryNormalizer;
        let result = normalizer.normalize(&payload);
        assert!(matches!(result, Err(NormalizerError::MissingField(_))));
    }

    #[test]
    fn missing_token_counts_map_to_unknown_not_rejection() {
        // Story #31 AC6: a payload missing token counts is stored with cost
        // explicitly unknown, not rejected and not defaulted to zero.
        let mut payload = valid_payload();
        let attributes = payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"]
            .as_array_mut()
            .expect("attributes array");
        attributes.retain(|a| {
            a.get("key").and_then(|k| k.as_str()) != Some("tokens.input")
                && a.get("key").and_then(|k| k.as_str()) != Some("tokens.output")
        });

        let normalizer = FoundryNormalizer;
        let result = normalizer
            .normalize(&payload)
            .expect("normalize must succeed");
        let exec = &result.executions[0];
        assert_eq!(exec.model_calls[0].input_tokens, None);
        assert_eq!(exec.model_calls[0].output_tokens, None);
    }

    #[test]
    fn absent_user_email_is_tolerated() {
        let mut payload = valid_payload();
        let resource_attrs = payload["resourceSpans"][0]["resource"]["attributes"]
            .as_array_mut()
            .expect("resource attributes array");
        resource_attrs.retain(|a| a.get("key").and_then(|k| k.as_str()) != Some("user.email"));

        let normalizer = FoundryNormalizer;
        let result = normalizer
            .normalize(&payload)
            .expect("normalize must succeed without user.email");
        let exec = &result.executions[0];
        assert_eq!(exec.user_email, None);
    }

    #[test]
    fn multiple_tool_calls_have_unique_deterministic_span_ids() {
        // The idempotency key is (trace_id, span_id) and must be unique per
        // row: the model call and every tool call need their own span_id.
        // Reprocessing the same payload must not change row counts.
        let mut payload = valid_payload();
        let events = payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["events"]
            .as_array_mut()
            .expect("events array");
        events.push(json!({
            "name": "tool.call",
            "attributes": [
                { "key": "tool.name", "value": { "stringValue": "read" } },
                { "key": "duration.ms", "value": { "intValue": "250" } }
            ]
        }));

        let normalizer = FoundryNormalizer;
        let result = normalizer.normalize(&payload).expect("normalize");
        let exec = &result.executions[0];

        assert_eq!(exec.tool_calls.len(), 2);
        let span_ids: Vec<&str> = exec.tool_calls.iter().map(|t| t.span_id.as_str()).collect();
        assert_ne!(
            span_ids[0], span_ids[1],
            "tool calls must have unique span_ids"
        );

        let again = normalizer.normalize(&payload).expect("normalize again");
        let again_ids: Vec<String> = again.executions[0]
            .tool_calls
            .iter()
            .map(|t| t.span_id.clone())
            .collect();
        let original_ids: Vec<String> = exec.tool_calls.iter().map(|t| t.span_id.clone()).collect();
        assert_eq!(
            again_ids, original_ids,
            "child span_ids must be deterministic across reprocessing"
        );
    }

    #[test]
    fn events_present_but_wrong_type_rejects_rather_than_dropping_tool_calls() {
        // A malformed `events` field (a string instead of an array) must
        // reject, not be silently treated as "zero tool calls" -- that is
        // structurally indistinguishable from a span that legitimately
        // called no tools, and a silently-dropped tool call is invisible
        // downstream forever.
        let mut payload = valid_payload();
        payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["events"] = json!("not-an-array");

        let normalizer = FoundryNormalizer;
        let result = normalizer.normalize(&payload);
        assert!(
            matches!(result, Err(NormalizerError::InvalidFieldType { .. })),
            "expected InvalidFieldType, got {result:?}"
        );
    }

    #[test]
    fn duration_overflow_rejects_rather_than_wrapping() {
        // Both timestamps come from `span_i64`, which accepts any parseable
        // i64 with no bounds check, including negatives (proto3 JSON
        // int64-as-string carries no sign restriction). A naive subtraction
        // panics under overflow-checks (debug/test) and silently wraps under
        // `[profile.prod]` (overflow-checks off, inherited from `release`) --
        // neither is an acceptable outcome for a cost-ledger input.
        let mut payload = valid_payload();
        let span = &mut payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        span["startTimeUnixNano"] = json!("-9223372036854775808");
        span["endTimeUnixNano"] = json!("9223372036854775807");

        let normalizer = FoundryNormalizer;
        let result = normalizer.normalize(&payload);
        assert!(
            matches!(result, Err(NormalizerError::InvalidDuration { .. })),
            "expected InvalidDuration, got {result:?}"
        );
    }

    #[test]
    fn end_before_start_rejects_rather_than_a_negative_duration() {
        let mut payload = valid_payload();
        let span = &mut payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        span["startTimeUnixNano"] = json!("1700000005000000000");
        span["endTimeUnixNano"] = json!("1700000000000000000");

        let normalizer = FoundryNormalizer;
        let result = normalizer.normalize(&payload);
        assert!(
            matches!(result, Err(NormalizerError::InvalidDuration { .. })),
            "expected InvalidDuration, got {result:?}"
        );
    }
}
