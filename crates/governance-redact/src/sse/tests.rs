//! Tests for [`super`]. Split into its own file (issue #175) rather than
//! raising the already-grandfathered LoC ceiling -- the same move
//! `otel/tests.rs` made, applied to the whole `mod tests` block.

use serde_json::Value;

use super::{SseEmit, SseHoldBack};
use crate::{engine::Engine, profile::Profile};

fn engine() -> Engine {
    Engine::new(Profile::coding_assistant(), "salt").expect("engine")
}

fn frame(json: &str) -> String {
    format!("data: {json}\n\n")
}

/// Concatenates delta content the way any real client does — same
/// helper shape as `streaming::tests::concat_deltas`, duplicated rather
/// than shared across a `#[cfg(test)]` boundary between modules.
fn concat_deltas(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        let Some(p) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if p == "[DONE]" || p.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(p) else {
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

/// Concatenates one tool call's `function.arguments` fragments, keyed by
/// that tool call's own `index` — the sibling of [`concat_deltas`] for
/// tool-call arguments instead of `content`.
fn concat_tool_call_args(body: &str, call_index: u64) -> String {
    let mut out = String::new();
    for line in body.lines() {
        let Some(p) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if p == "[DONE]" || p.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(p) else {
            continue;
        };
        let Some(choices) = v.get("choices").and_then(|c| c.as_array()) else {
            continue;
        };
        for choice in choices {
            let Some(calls) = choice
                .get("delta")
                .and_then(|d| d.get("tool_calls"))
                .and_then(|t| t.as_array())
            else {
                continue;
            };
            for call in calls {
                let idx = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                if idx != call_index {
                    continue;
                }
                if let Some(s) = call
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                {
                    out.push_str(s);
                }
            }
        }
    }
    out
}

fn drive(hold: &mut SseHoldBack, engine: &Engine, chunks: &[&str]) -> String {
    let mut out = String::new();
    for c in chunks {
        match hold.push(engine, c).expect("push") {
            SseEmit::Release(s) => out.push_str(&s),
            SseEmit::Nothing => {}
            SseEmit::Blocked(e) => panic!("unexpected block: {e:?}"),
        }
    }
    match hold.flush(engine).expect("flush") {
        SseEmit::Release(s) => out.push_str(&s),
        SseEmit::Nothing => {}
        SseEmit::Blocked(e) => panic!("unexpected block: {e:?}"),
    }
    out
}

#[test]
fn clean_stream_round_trips_its_text() {
    let e = engine();
    let mut h = SseHoldBack::with_window(64);
    let body = drive(
        &mut h,
        &e,
        &[
            &frame(r#"{"choices":[{"index":0,"delta":{"content":"let x"}}]}"#),
            &frame(r#"{"choices":[{"index":0,"delta":{"content":" = 1;"}}]}"#),
        ],
    );
    assert_eq!(concat_deltas(&body), "let x = 1;");
    assert_eq!(h.redactions(), 0);
}

/// The whole point of this module: with a window shorter than the
/// entity, a naive byte-cut would split a frame's content across two
/// release batches. The frame-boundary snap must prevent that,
/// verified here by checking not just the concatenated text (which
/// `HoldBack` already gets right) but that no PARTIAL frame content
/// escapes before the whole entity is resolved.
#[test]
fn entity_split_across_frames_is_redacted_not_leaked() {
    let e = engine();
    let mut h = SseHoldBack::with_window(4); // shorter than "jane.doe@example.com"
    let body = drive(
        &mut h,
        &e,
        &[
            &frame(r#"{"choices":[{"index":0,"delta":{"content":"mail jane.doe@ex"}}]}"#),
            &frame(r#"{"choices":[{"index":0,"delta":{"content":"ample.com now"}}]}"#),
        ],
    );
    let text = concat_deltas(&body);
    assert!(
        !text.contains("jane.doe@example.com"),
        "email leaked: {text}"
    );
    assert!(!text.contains("jane.doe@ex"), "fragment leaked: {text}");
    assert!(text.contains("mail"), "surrounding text lost: {text}");
}

#[test]
fn split_credential_blocks_the_stream() {
    let e = engine();
    let mut h = SseHoldBack::with_window(64);
    h.push(
        &e,
        &frame(r#"{"choices":[{"index":0,"delta":{"content":"ghp_abcdefghijkl"}}]}"#),
    )
    .expect("push");
    let out = h
        .push(
            &e,
            &frame(r#"{"choices":[{"index":0,"delta":{"content":"mnopqrstuvwxyz0123456789"}}]}"#),
        )
        .expect("push");
    match out {
        SseEmit::Blocked(entities) => {
            assert!(
                entities.iter().any(|s| s.contains("Secret")),
                "{entities:?}"
            );
        }
        other => panic!("expected the split token to block, got {other:?}"),
    }
}

#[test]
fn non_content_fields_survive() {
    let e = engine();
    let mut h = SseHoldBack::with_window(64);
    let body = drive(
        &mut h,
        &e,
        &[
            &frame(
                r#"{"id":"c1","model":"glm","choices":[{"index":0,"delta":{"role":"assistant"}}]}"#,
            ),
            &frame(
                r#"{"id":"c1","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}]}"#,
            ),
            &frame(
                r#"{"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"total_tokens":5}}"#,
            ),
        ],
    );
    assert!(body.contains("\"role\":\"assistant\""), "{body}");
    assert!(body.contains("\"finish_reason\":\"stop\""), "{body}");
    assert!(body.contains("\"total_tokens\":5"), "{body}");
    assert!(body.contains("\"model\":\"glm\""), "{body}");
}

#[test]
fn multiple_choices_do_not_bleed_into_each_other() {
    let e = engine();
    let mut h = SseHoldBack::with_window(64);
    let body = drive(
        &mut h,
        &e,
        &[&frame(
            r#"{"choices":[{"index":0,"delta":{"content":"all fine here"}},{"index":1,"delta":{"content":"write jane.doe@example.com"}}]}"#,
        )],
    );
    assert!(
        body.contains("all fine here"),
        "clean choice altered: {body}"
    );
    assert!(
        !body.contains("jane.doe@example.com"),
        "dirty choice not redacted: {body}"
    );
}

#[test]
fn malformed_data_line_is_passed_through_not_dropped() {
    let e = engine();
    let mut h = SseHoldBack::with_window(64);
    let body = drive(&mut h, &e, &["data: not json at all\n\n"]);
    assert!(body.contains("not json at all"), "{body}");
}

#[test]
fn frame_arrives_split_across_two_pushes() {
    // The line assembler must reassemble a frame whose bytes are split
    // mid-line by the transport, not just mid-frame at a `\n` boundary.
    let e = engine();
    let mut h = SseHoldBack::with_window(64);
    let body = drive(
        &mut h,
        &e,
        &[
            r#"data: {"choices":[{"index":0,"delta":{"content":"hi"}}"#,
            "]}\n\n",
        ],
    );
    assert_eq!(concat_deltas(&body), "hi");
}

#[test]
fn released_prefix_never_splits_a_single_frames_content() {
    // Directly checks the property `HoldBack` cannot offer: every
    // released `data:` line's content is either the full redacted text
    // or empty -- never a byte-level fragment of what that specific
    // frame originally carried.
    let e = engine();
    let mut h = SseHoldBack::with_window(2);
    let body = drive(
        &mut h,
        &e,
        &[
            &frame(r#"{"choices":[{"index":0,"delta":{"content":"abcdefghij"}}]}"#),
            &frame(r#"{"choices":[{"index":0,"delta":{"content":"klmnopqrst"}}]}"#),
        ],
    );
    // A window of 2 is shorter than either frame's own 10-byte content,
    // so each frame is already past the window the moment the NEXT
    // frame arrives — they are free to release individually rather
    // than merge into one batch. The invariant under test is not "one
    // specific batch shape" but "never a byte-fragment of what a frame
    // originally carried": every non-empty value must be a WHOLE
    // frame's content, alone or concatenated with whichever neighbours
    // shared its release batch — never a prefix/suffix of one.
    let valid = ["", "abcdefghij", "klmnopqrst", "abcdefghijklmnopqrst"];
    for line in body.lines() {
        let Some(p) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        let v: Value = serde_json::from_str(p).expect("json");
        let content = v["choices"][0]["delta"]["content"].as_str().unwrap_or("");
        assert!(
            valid.contains(&content),
            "frame content was a byte-fragment of a frame's original text: {content:?}"
        );
    }
    assert_eq!(concat_deltas(&body), "abcdefghijklmnopqrst");
}

// ── Tool-call arguments: the P0 this module used to miss entirely. ─────
// `delta_contents` extracted only `delta.content`, so a credential
// riding in `delta.tool_calls[].function.arguments` was invisible to
// the incremental scanner and rode straight through to the client.

#[test]
fn tool_call_arguments_split_across_frames_block_the_stream() {
    let e = engine();
    let mut h = SseHoldBack::with_window(64);
    h.push(
        &e,
        &frame(
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ghp_abcdefghijkl"}}]}}]}"#,
        ),
    )
    .expect("push");
    let out = h
        .push(
            &e,
            &frame(
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"mnopqrstuvwxyz0123456789"}}]}}]}"#,
            ),
        )
        .expect("push");
    match out {
        SseEmit::Blocked(entities) => {
            assert!(
                entities.iter().any(|s| s.contains("Secret")),
                "{entities:?}"
            );
        }
        other => panic!("expected the split tool_call token to block, got {other:?}"),
    }
}

/// Adversarial chunking: the same credential as above, but fed one byte
/// at a time. Proves the fix does not depend on convenient chunk
/// boundaries -- if tool-call arguments were still unscanned, this would
/// simply reassemble and release the whole credential with nothing ever
/// blocking.
#[test]
fn tool_call_arguments_leak_via_one_byte_chunks_is_caught() {
    let e = engine();
    let mut h = SseHoldBack::with_window(64);
    let full = frame(
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ghp_abcdefghijklmnopqrstuvwxyz0123456789"}}]}}]}"#,
    );
    let mut blocked = false;
    let mut released = String::new();
    for byte in full.as_bytes() {
        let chunk = std::str::from_utf8(std::slice::from_ref(byte))
            .expect("this frame is pure ASCII, so every single byte is valid UTF-8 alone");
        match h.push(&e, chunk).expect("push") {
            SseEmit::Blocked(entities) => {
                blocked = true;
                assert!(
                    entities.iter().any(|s| s.contains("Secret")),
                    "{entities:?}"
                );
                break;
            }
            SseEmit::Release(s) => released.push_str(&s),
            SseEmit::Nothing => {}
        }
    }
    assert!(
        blocked,
        "one-byte-chunked tool_call argument credential was not blocked; released={released:?}"
    );
}

#[test]
fn tool_call_arguments_pii_split_across_frames_is_redacted_not_leaked() {
    let e = engine();
    let mut h = SseHoldBack::with_window(4); // shorter than the email
    let body = drive(
        &mut h,
        &e,
        &[
            &frame(
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"mail jane.doe@ex"}}]}}]}"#,
            ),
            &frame(
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ample.com now"}}]}}]}"#,
            ),
        ],
    );
    let args = concat_tool_call_args(&body, 0);
    assert!(
        !args.contains("jane.doe@example.com"),
        "email leaked: {args}"
    );
    assert!(!args.contains("jane.doe@ex"), "fragment leaked: {args}");
    assert!(args.contains("mail"), "surrounding text lost: {args}");
}

// ── Malformed `data:` payloads: must be scanned, never released
//    unexamined. ─────────────────────────────────────────────────────

#[test]
fn malformed_data_line_with_secret_is_blocked() {
    let e = engine();
    let mut h = SseHoldBack::with_window(64);
    // Missing the final closing brace: invalid JSON, but the credential
    // inside is plainly there for a text scan to find.
    let out = h
        .push(
            &e,
            "data: {\"choices\":[{\"delta\":{\"content\":\"ghp_abcdefghijklmnopqrstuvwxyz0123456789\"}}]\n\n",
        )
        .expect("push");
    match out {
        SseEmit::Blocked(entities) => {
            assert!(
                entities.iter().any(|s| s.contains("Secret")),
                "{entities:?}"
            );
        }
        other => panic!("expected the malformed frame's credential to block, got {other:?}"),
    }
}
