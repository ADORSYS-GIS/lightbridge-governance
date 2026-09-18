//! Tests for [`super`]. Split into its own file (issue #175) rather than
//! raising the already-grandfathered LoC ceiling -- the same move
//! `otel/tests.rs` made, applied to the whole `mod tests` block.

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
    assert_eq!(exec.model_calls[0].input_tokens, Some(1000));
    assert_eq!(exec.model_calls[0].output_tokens, Some(500));
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

#[test]
fn events_present_but_wrong_type_rejects_rather_than_dropping_tool_calls() {
    // A malformed `events` field (a string instead of an array) must
    // reject, not be silently treated as "zero tool calls" -- that is
    // structurally indistinguishable from a span that legitimately
    // called no tools, and a silently-dropped tool call is invisible
    // downstream forever.
    let mut payload = valid_payload();
    payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["events"] = json!("not-an-array");

    let normalizer = ClaudeCodeNormalizer;
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

    let normalizer = ClaudeCodeNormalizer;
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

    let normalizer = ClaudeCodeNormalizer;
    let result = normalizer.normalize(&payload);
    assert!(
        matches!(result, Err(NormalizerError::InvalidDuration { .. })),
        "expected InvalidDuration, got {result:?}"
    );
}
