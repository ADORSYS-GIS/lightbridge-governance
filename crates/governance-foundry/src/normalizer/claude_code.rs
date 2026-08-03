//! Claude Code normalizer (#32).
//!
//! Claude Code emits OTLP spans with the following structure:
//! - Resource attributes: `user.email`, `service.name` = "claude-code"
//! - Span attributes: `session.id`, `model.name`, `tokens.input`, `tokens.output`
//! - Events: `tool.call` with `tool.name` and `duration.ms`
//!
//! Attributes arrive as real OTLP proto3 JSON (`attributes` arrays, string
//! encoded ints) -- parsed via the shared [`super::otlp`] helpers.

use chrono::{DateTime, Utc};
use governance_core::ingest::{ExecutionInput, ModelCallInput, ToolCallInput};

use super::{
    Normalizer, NormalizerError, TelemetryPayload,
    otlp::{attr_i64, attr_string, span_i64, span_string},
};

pub struct ClaudeCodeNormalizer;

impl Normalizer for ClaudeCodeNormalizer {
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
    // feature uses it yet), but it is still validated so a malformed value is
    // a hard rejection rather than silently ignored telemetry.
    attr_string(span, "session.id", "span")?;
    let model_name = attr_string(span, "model.name", "span")?
        .ok_or_else(|| NormalizerError::MissingField("model.name".to_owned()))?;
    let input_tokens = attr_i64(span, "tokens.input", "span")?
        .ok_or_else(|| NormalizerError::MissingField("tokens.input".to_owned()))?;
    let output_tokens = attr_i64(span, "tokens.output", "span")?
        .ok_or_else(|| NormalizerError::MissingField("tokens.output".to_owned()))?;

    let start_time_unix_nano = span_i64(span, "startTimeUnixNano")?;
    let end_time_unix_nano = span_i64(span, "endTimeUnixNano")?;

    let started_at = DateTime::<Utc>::from_timestamp_nanos(start_time_unix_nano);
    let duration_ms = (end_time_unix_nano - start_time_unix_nano) / 1_000_000;

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

    if let Some(events) = span.get("events").and_then(|v| v.as_array()) {
        for (idx, event) in events.iter().enumerate() {
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
    /// encoded ints).
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
                            { "key": "model.name", "value": { "stringValue": "claude-3-sonnet" } },
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
    fn normalizes_valid_claude_code_payload() {
        let normalizer = ClaudeCodeNormalizer;
        let result = normalizer.normalize(&valid_payload()).expect("normalize");

        assert_eq!(result.executions.len(), 1);
        let exec = &result.executions[0];
        assert_eq!(exec.trace_id, "trace-123");
        assert_eq!(exec.span_id, "span-456");
        assert_eq!(exec.user_email, Some("user@example.com".to_owned()));
        assert_eq!(exec.duration_ms, 5000);
        assert_eq!(exec.model_calls.len(), 1);
        assert_eq!(exec.model_calls[0].model, "claude-3-sonnet");
        assert_eq!(exec.model_calls[0].input_tokens, 1000);
        assert_eq!(exec.model_calls[0].output_tokens, 500);
        assert_eq!(exec.tool_calls.len(), 1);
        assert_eq!(exec.tool_calls[0].tool_name, "bash");
        assert_eq!(exec.tool_calls[0].duration_ms, 1500);
    }

    /// The idempotency key is (trace_id, span_id) and must be unique per row:
    /// the model call and every tool call need their own span_id, derived
    /// deterministically from the parent span. Without this, two tool calls
    /// under one execution collide on the unique index and only one is stored.
    #[test]
    fn child_rows_have_distinct_deterministic_span_ids() {
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

        let normalizer = ClaudeCodeNormalizer;
        let result = normalizer.normalize(&payload).expect("normalize");
        let exec = &result.executions[0];

        assert_eq!(exec.tool_calls.len(), 2);
        let span_ids: Vec<&str> = exec.tool_calls.iter().map(|t| t.span_id.as_str()).collect();
        let model_span_id = exec.model_calls[0].span_id.as_str();

        assert_ne!(
            span_ids[0], span_ids[1],
            "two tool calls must not share a span_id"
        );
        assert_ne!(model_span_id, span_ids[0]);
        assert_ne!(model_span_id, span_ids[1]);

        // Deterministic: normalizing twice yields the same span_ids, so
        // reprocessing upserts the same rows rather than creating new ones.
        let again = normalizer.normalize(&payload).expect("normalize again");
        let again_ids: Vec<String> = again.executions[0]
            .tool_calls
            .iter()
            .map(|t| t.span_id.clone())
            .collect();
        let original_ids: Vec<String> = exec.tool_calls.iter().map(|t| t.span_id.clone()).collect();
        assert_eq!(
            again_ids, original_ids,
            "child span_ids must be deterministic"
        );
    }

    #[test]
    fn rejects_missing_resource_spans() {
        let normalizer = ClaudeCodeNormalizer;
        let result = normalizer.normalize(&json!({}));
        assert!(matches!(result, Err(NormalizerError::MissingField(_))));
    }

    #[test]
    fn rejects_missing_trace_id() {
        let mut payload = valid_payload();
        payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0]
            .as_object_mut()
            .expect("span object")
            .remove("traceId");

        let normalizer = ClaudeCodeNormalizer;
        let result = normalizer.normalize(&payload);
        assert!(matches!(result, Err(NormalizerError::MissingField(_))));
    }
}
