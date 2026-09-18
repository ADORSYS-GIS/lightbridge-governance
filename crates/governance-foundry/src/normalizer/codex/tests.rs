//! Tests for [`super`]. Split into its own file (issue #175) rather than
//! raising the already-grandfathered LoC ceiling -- the same move
//! `otel/tests.rs` made, applied to the whole `mod tests` block.

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
    attributes.retain(|a| a.get("key").and_then(|k| k.as_str()) != Some("codex.turn.input_tokens"));
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

#[test]
fn events_present_but_wrong_type_rejects_rather_than_dropping_tool_calls() {
    // A malformed `events` field (a string instead of an array) must
    // reject, not be silently treated as "zero tool calls" -- that is
    // structurally indistinguishable from a span that legitimately
    // called no tools, and a silently-dropped tool call is invisible
    // downstream forever.
    let mut payload = valid_payload();
    payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["events"] = json!("not-an-array");

    let normalizer = CodexNormalizer;
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

    let normalizer = CodexNormalizer;
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

    let normalizer = CodexNormalizer;
    let result = normalizer.normalize(&payload);
    assert!(
        matches!(result, Err(NormalizerError::InvalidDuration { .. })),
        "expected InvalidDuration, got {result:?}"
    );
}
