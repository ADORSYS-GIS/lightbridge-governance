//! Tests for [`super`]. Split into its own file (issue #175) rather than
//! raising the already-grandfathered LoC ceiling -- the same move
//! `otel/tests.rs` made, applied to the whole `mod tests` block.

use super::scan_sse;
use crate::{engine::Engine, profile::Profile};

fn engine() -> Engine {
    Engine::new(Profile::coding_assistant(), "salt").expect("engine")
}

fn sse(chunks: &[&str]) -> String {
    let mut s = String::new();
    for c in chunks {
        s.push_str("data: ");
        s.push_str(c);
        s.push_str("\n\n");
    }
    s.push_str("data: [DONE]\n\n");
    s
}

/// Concatenates delta content the way any real client does.
fn concat_deltas(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        let Some(p) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if p == "[DONE]" || p.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(p) else {
            continue;
        };
        if let Some(choices) = v.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                if let Some(s) = choice
                    .get("delta")
                    .and_then(|d| d.get("content"))
                    .and_then(|c| c.as_str())
                {
                    out.push_str(s);
                }
            }
        }
    }
    out
}

#[test]
fn clean_stream_round_trips_its_text() {
    let e = engine();
    let body = sse(&[
        r#"{"choices":[{"index":0,"delta":{"content":"let x"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":" = 1;"}}]}"#,
    ]);
    let out = scan_sse(&e, &body).expect("scan");
    assert_eq!(concat_deltas(&out.body), "let x = 1;");
    assert_eq!(out.report.redactions, 0);
}

#[test]
fn done_sentinel_is_preserved() {
    let e = engine();
    let body = sse(&[r#"{"choices":[{"index":0,"delta":{"content":"hi"}}]}"#]);
    let out = scan_sse(&e, &body).expect("scan");
    assert!(
        out.body.contains("data: [DONE]"),
        "stream must still terminate"
    );
}

#[test]
fn entity_split_across_chunks_is_still_caught() {
    // THE reason buffered mode exists. Neither chunk contains a full
    // address; only the coalesced text does.
    let e = engine();
    let body = sse(&[
        r#"{"choices":[{"index":0,"delta":{"content":"mail jane@ex"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":"ample.com now"}}]}"#,
    ]);
    let out = scan_sse(&e, &body).expect("scan");
    let text = concat_deltas(&out.body);
    assert!(
        !text.contains("jane@example.com"),
        "split entity survived: {text}"
    );
    assert_eq!(out.report.redactions, 1);
}

#[test]
fn credential_split_across_chunks_blocks_the_stream() {
    let e = engine();
    let body = sse(&[
        r#"{"choices":[{"index":0,"delta":{"content":"ghp_abcdefghijkl"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":"mnopqrstuvwxyz0123456789"}}]}"#,
    ]);
    let out = scan_sse(&e, &body).expect("scan");
    assert!(out.report.is_blocked(), "split credential must block");
    assert!(out.body.is_empty(), "blocked stream must emit nothing");
}

#[test]
fn non_content_fields_survive() {
    let e = engine();
    let body = sse(&[
        r#"{"id":"c1","model":"glm","choices":[{"index":0,"delta":{"role":"assistant"}}]}"#,
        r#"{"id":"c1","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}]}"#,
        r#"{"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"total_tokens":5}}"#,
    ]);
    let out = scan_sse(&e, &body).expect("scan");
    assert!(out.body.contains("\"role\":\"assistant\""));
    assert!(out.body.contains("\"finish_reason\":\"stop\""));
    assert!(out.body.contains("\"total_tokens\":5"));
    assert!(out.body.contains("\"model\":\"glm\""));
}

#[test]
fn multiple_choices_do_not_bleed_into_each_other() {
    // n > 1: choice 0 is clean, choice 1 has an address. Coalescing them
    // together would redact the wrong stream.
    let e = engine();
    let body = sse(&[
        r#"{"choices":[{"index":0,"delta":{"content":"all fine here"}},{"index":1,"delta":{"content":"write jane@example.com"}}]}"#,
    ]);
    let out = scan_sse(&e, &body).expect("scan");
    assert!(out.body.contains("all fine here"), "clean choice altered");
    assert!(
        !out.body.contains("jane@example.com"),
        "dirty choice not redacted"
    );
}

#[test]
fn malformed_data_line_is_passed_through_not_dropped() {
    let e = engine();
    let body = "data: not json at all\n\ndata: [DONE]\n\n";
    let out = scan_sse(&e, body).expect("scan");
    assert!(out.body.contains("not json at all"));
    assert!(out.body.contains("[DONE]"));
}

#[test]
fn empty_stream_is_handled() {
    let e = engine();
    let out = scan_sse(&e, "").expect("scan");
    assert!(out.body.is_empty());
    assert_eq!(out.report.redactions, 0);
}

#[test]
fn redacted_text_appears_exactly_once() {
    // The redistribution rule: first chunk gets the whole string, the rest
    // are blanked. Concatenation must not duplicate it.
    let e = engine();
    let body = sse(&[
        r#"{"choices":[{"index":0,"delta":{"content":"a@b.com"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":" and more"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":" text"}}]}"#,
    ]);
    let out = scan_sse(&e, &body).expect("scan");
    let text = concat_deltas(&out.body);
    assert!(!text.contains("a@b.com"));
    assert_eq!(
        text.matches("and more").count(),
        1,
        "content duplicated on redistribution: {text}"
    );
}

// ── Tool-call arguments: the P0 this module used to miss entirely. ─────
// `delta_contents` extracted only `delta.content`; `delta.tool_calls[].
// function.arguments` was invisible to the scanner, so a credential
// riding in a tool call rode straight through as "no redactable
// content". These tests would fail against that code for exactly that
// reason: `report.is_blocked()` false and the secret present verbatim
// in `out.body`.

#[test]
fn tool_call_arguments_with_credential_block_the_stream() {
    let e = engine();
    let body = sse(&[
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ghp_abcdefghijklmnopqrstuvwxyz0123456789"}}]}}]}"#,
    ]);
    let out = scan_sse(&e, &body).expect("scan");
    assert!(
        out.report.is_blocked(),
        "credential in tool_call arguments must block: {:?}",
        out.report
    );
    assert!(out.body.is_empty(), "blocked stream must emit nothing");
}

#[test]
fn tool_call_arguments_pii_is_redacted_not_leaked() {
    let e = engine();
    let body = sse(&[
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"to\":\"jane@example.com\"}"}}]}}]}"#,
    ]);
    let out = scan_sse(&e, &body).expect("scan");
    assert_eq!(out.report.redactions, 1);
    assert!(
        !out.body.contains("jane@example.com"),
        "tool_call argument leaked: {}",
        out.body
    );
}

#[test]
fn tool_call_arguments_and_content_are_independent_streams() {
    // Redacting one must not touch the other -- they are different
    // ContentField keys under the same choice index.
    let e = engine();
    let body = sse(&[
        r#"{"choices":[{"index":0,"delta":{"content":"all fine here","tool_calls":[{"index":0,"function":{"arguments":"mail jane@example.com"}}]}}]}"#,
    ]);
    let out = scan_sse(&e, &body).expect("scan");
    assert!(
        out.body.contains("all fine here"),
        "clean content wrongly altered: {}",
        out.body
    );
    assert!(
        !out.body.contains("jane@example.com"),
        "tool_call argument leaked: {}",
        out.body
    );
}

// ── Malformed `data:` payloads: must be scanned, never released
//    unexamined. A parse failure is "unknown", and unknown routes to
//    the strictest branch. ─────────────────────────────────────────────

#[test]
fn malformed_data_line_with_secret_is_not_leaked() {
    let e = engine();
    // Missing the final closing brace: invalid JSON, but the credential
    // inside is plainly there for a text scan to find.
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"ghp_abcdefghijklmnopqrstuvwxyz0123456789\"}}]\n\ndata: [DONE]\n\n";
    let out = scan_sse(&e, body).expect("scan");
    assert!(
        out.report.is_blocked(),
        "credential in malformed data frame must block: {:?}",
        out.report
    );
    assert!(out.body.is_empty(), "blocked stream must emit nothing");
}
