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
    otlp::{attr_i64, attr_string, duration_ms, events, span_i64, span_string},
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
mod tests;
