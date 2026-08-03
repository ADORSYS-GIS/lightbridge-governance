//! OpenAI Codex normalizer (#33).
//!
//! Codex emits OTLP spans with the following structure:
//! - Resource attributes: `user.email`, `service.name` = "codex"
//! - Span attributes: `session.id`, `model.name`, `codex.turn.input_tokens`, `codex.turn.output_tokens`
//! - Events: `tool.call` with `tool.name` and `duration.ms`
//!
//! Note: Codex token counts appear as span attributes, not as metrics (#33668).
//! This normalizer extracts them from the span attributes. Attributes arrive as
//! real OTLP proto3 JSON (attribute arrays, string encoded ints).

use chrono::{DateTime, Utc};
use governance_core::ingest::{ExecutionInput, ModelCallInput, ToolCallInput};

use super::otlp::{attr_i64, attr_string, span_i64, span_string};
use super::{Normalizer, NormalizerError, TelemetryPayload};

pub struct CodexNormalizer;

impl Normalizer for CodexNormalizer {
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
    let input_tokens = attr_i64(span, "codex.turn.input_tokens", "span")?
        .ok_or_else(|| NormalizerError::MissingField("codex.turn.input_tokens".to_owned()))?;
    let output_tokens = attr_i64(span, "codex.turn.output_tokens", "span")?
        .ok_or_else(|| NormalizerError::MissingField("codex.turn.output_tokens".to_owned()))?;

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
                            { "key": "codex.turn.input_tokens", "value": { "intValue": "1000" } },
                            { "key": "codex.turn.output_tokens", "value": { "intValue": "500" } }
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
    fn normalizes_valid_codex_payload() {
        let normalizer = CodexNormalizer;
        let result = normalizer.normalize(&valid_payload()).expect("normalize");

        assert_eq!(result.executions.len(), 1);
        let exec = &result.executions[0];
        assert_eq!(exec.trace_id, "trace-123");
        assert_eq!(exec.span_id, "span-456");
        assert_eq!(exec.user_email, Some("user@example.com".to_owned()));
        assert_eq!(exec.duration_ms, 5000);
        assert_eq!(exec.model_calls.len(), 1);
        assert_eq!(exec.model_calls[0].model, "gpt-4");
        assert_eq!(exec.model_calls[0].input_tokens, 1000);
        assert_eq!(exec.model_calls[0].output_tokens, 500);
        assert_eq!(exec.tool_calls.len(), 1);
        assert_eq!(exec.tool_calls[0].tool_name, "bash");
        assert_eq!(exec.tool_calls[0].duration_ms, 1500);
    }

    #[test]
    fn rejects_missing_resource_spans() {
        let normalizer = CodexNormalizer;
        let result = normalizer.normalize(&json!({}));
        assert!(matches!(result, Err(NormalizerError::MissingField(_))));
    }

    #[test]
    fn rejects_missing_token_counts() {
        let mut payload = valid_payload();
        let attributes = payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"]
            .as_array_mut()
            .expect("attributes array");
        attributes
            .retain(|a| a.get("key").and_then(|k| k.as_str()) != Some("codex.turn.input_tokens"));

        let normalizer = CodexNormalizer;
        let result = normalizer.normalize(&payload);
        assert!(matches!(result, Err(NormalizerError::MissingField(_))));
    }
}
