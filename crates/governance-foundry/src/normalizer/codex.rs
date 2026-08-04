//! OpenAI Codex normalizer (#33).
//!
//! ## Token Count Extraction
//!
//! Codex emits token counts differently depending on execution mode:
//!
//! - **Interactive mode**: Token counts are on **metrics** (`codex.turn.token_usage`), not spans.
//!   Metrics don't carry identity (`user.email`), so we cannot join them to spans.
//!   Interactive sessions will have **unknown cost** until we implement metric-span joining.
//!
//! - **Exec mode**: Token counts are on spans as `input_token_count`, `output_token_count`
//!   (verified in source: `codex-rs/otel/src/events/shared.rs:13-17`).
//!
//! This normalizer attempts to extract token counts from span attributes using multiple
//! possible names to handle both cases:
//! 1. `input_token_count` / `output_token_count` (verified for exec mode)
//! 2. `codex.turn.input_tokens` / `codex.turn.output_tokens` (fallback, unverified)
//!
//! ## Identity
//!
//! Codex emits `user.email` on **log events only**, not on spans or metrics.
//! Identity must come from the per-developer ingest token, not the payload.
//! The payload's `user.email` is a cross-check, not the source of truth.
//!
//! ## What This Normalizer Does
//!
//! - Extracts execution metadata from spans (trace_id, span_id, model, duration)
//! - Extracts token counts from span attributes (works for exec mode)
//! - Extracts tool calls from span events
//! - Tolerates missing `user.email` (API-key auth)
//!
//! ## What This Normalizer Cannot Do (Yet)
//!
//! - Extract token counts for interactive sessions (they're on metrics, not spans)
//! - Join metrics to spans by trace_id (not implemented)
//!
//! Attributes arrive as real OTLP proto3 JSON (attribute arrays, string encoded ints).

use chrono::{DateTime, Utc};
use governance_core::ingest::{ExecutionInput, ModelCallInput, ToolCallInput};

use super::{
    Normalizer, NormalizerError, TelemetryPayload,
    otlp::{attr_i64, attr_string, span_i64, span_string},
};

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
    // Token counts are optional: a span that omits them yields a model call
    // with unknown cost (story #31 AC6), not a rejection.
    //
    // Codex emits token counts on spans only in exec mode (issue #33668).
    // Interactive mode token counts are on metrics, which don't carry identity.
    // We check multiple attribute names to handle both cases:
    // - `input_token_count` / `output_token_count` (verified for exec mode)
    // - `codex.turn.input_tokens` / `codex.turn.output_tokens` (fallback, unverified)
    let input_tokens = match attr_i64(span, "input_token_count", "span")? {
        Some(v) => Some(v),
        None => attr_i64(span, "codex.turn.input_tokens", "span")?,
    };
    let output_tokens = match attr_i64(span, "output_token_count", "span")? {
        Some(v) => Some(v),
        None => attr_i64(span, "codex.turn.output_tokens", "span")?,
    };

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
        assert_eq!(exec.model_calls[0].input_tokens, Some(1000));
        assert_eq!(exec.model_calls[0].output_tokens, Some(500));
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
    fn missing_token_counts_map_to_unknown_not_rejection() {
        // Story #31 AC6: a payload missing token counts is stored with cost
        // explicitly unknown, not rejected and not defaulted to zero.
        let mut payload = valid_payload();
        let attributes = payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"]
            .as_array_mut()
            .expect("attributes array");
        attributes
            .retain(|a| a.get("key").and_then(|k| k.as_str()) != Some("codex.turn.input_tokens"));
        attributes
            .retain(|a| a.get("key").and_then(|k| k.as_str()) != Some("codex.turn.output_tokens"));

        let normalizer = CodexNormalizer;
        let result = normalizer
            .normalize(&payload)
            .expect("normalize must succeed");
        let exec = &result.executions[0];
        assert_eq!(exec.model_calls[0].input_tokens, None);
        assert_eq!(exec.model_calls[0].output_tokens, None);
    }

    #[test]
    fn absent_user_email_is_tolerated() {
        // Story #33: user.email is absent under API-key or custom-provider auth.
        // The normalizer must tolerate this and not reject the payload.
        let mut payload = valid_payload();
        let resource_attrs = payload["resourceSpans"][0]["resource"]["attributes"]
            .as_array_mut()
            .expect("resource attributes array");
        resource_attrs.retain(|a| a.get("key").and_then(|k| k.as_str()) != Some("user.email"));

        let normalizer = CodexNormalizer;
        let result = normalizer
            .normalize(&payload)
            .expect("normalize must succeed without user.email");
        let exec = &result.executions[0];
        assert_eq!(exec.user_email, None);
    }

    #[test]
    fn multiple_tool_calls_have_unique_span_ids() {
        // Story #33: multiple tool calls must have unique span_ids for idempotency.
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

        let normalizer = CodexNormalizer;
        let result = normalizer.normalize(&payload).expect("normalize");
        let exec = &result.executions[0];

        assert_eq!(exec.tool_calls.len(), 2);
        let span_ids: Vec<&str> = exec.tool_calls.iter().map(|t| t.span_id.as_str()).collect();
        assert_ne!(
            span_ids[0], span_ids[1],
            "tool calls must have unique span_ids"
        );
    }

    #[test]
    fn codex_exec_token_counts_from_span_attributes() {
        // Story #33: codex exec does not export codex.turn.token_usage metric (#33668).
        // Token counts appear as input_token_count / output_token_count attributes instead.
        // The normalizer must extract from these fallback attribute names.
        let mut payload = valid_payload();
        let attributes = payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"]
            .as_array_mut()
            .expect("attributes array");

        // Remove the interactive codex attributes
        attributes.retain(|a| {
            a.get("key").and_then(|k| k.as_str()) != Some("codex.turn.input_tokens")
                && a.get("key").and_then(|k| k.as_str()) != Some("codex.turn.output_tokens")
        });

        // Add exec-specific token count attributes
        attributes.push(json!({
            "key": "input_token_count",
            "value": { "intValue": "1500" }
        }));
        attributes.push(json!({
            "key": "output_token_count",
            "value": { "intValue": "750" }
        }));

        let normalizer = CodexNormalizer;
        let result = normalizer.normalize(&payload).expect("normalize");
        let exec = &result.executions[0];

        // Should extract from input_token_count/output_token_count
        assert_eq!(exec.model_calls[0].input_tokens, Some(1500));
        assert_eq!(exec.model_calls[0].output_tokens, Some(750));
    }
}
