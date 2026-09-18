//! Tests for the buffered (non-SSE) response path — the P0 this file used
//! to miss entirely. Every response chunk went into `SseHoldBack`, which
//! only ever looks inside `data:` lines — a `stream: false` JSON completion
//! has none, so it sailed through as `Frame::Passthrough` with zero calls to
//! `engine.scan` (issue #180 split).

use super::{super::*, engine, extract_body, metrics, response_state_with_content_type};

#[test]
fn non_streaming_json_response_with_secret_is_blocked_not_forwarded_unscanned() {
    let e = engine();
    let m = metrics();
    let mut state = response_state_with_content_type(64, "application/json");
    let body =
        r#"{"choices":[{"message":{"content":"here: ghp_abcdefghijklmnopqrstuvwxyz0123456789"}}]}"#;
    let resp = handle_response_chunk(&e, &m, &mut state, body.as_bytes(), true);
    match &resp.response {
        Some(Resp::ImmediateResponse(imm)) => {
            assert_eq!(
                imm.status.as_ref().map(|s| s.code),
                Some(StatusCode::UnprocessableEntity as i32),
                "expected a content_blocked refusal, got {imm:?}"
            );
        }
        other => panic!("expected the credential to block the non-SSE response, got {other:?}"),
    }
}

#[test]
fn non_streaming_json_response_with_pii_is_redacted_not_leaked() {
    let e = engine();
    let m = metrics();
    let mut state = response_state_with_content_type(64, "application/json");
    let body = r#"{"choices":[{"message":{"content":"it is jane@example.com"}}]}"#;
    let resp = handle_response_chunk(&e, &m, &mut state, body.as_bytes(), true);
    assert!(
        !matches!(&resp.response, Some(Resp::ImmediateResponse(_))),
        "PII-only response (redacted, not a credential) must not be refused: {resp:?}"
    );
    let out = extract_body(&resp).expect("redacted body forwarded");
    let out = String::from_utf8(out).expect("utf8");
    assert!(!out.contains("jane@example.com"), "leaked: {out}");
}

/// No `ResponseHeaders` message at all — exactly what a malfunctioning
/// upstream, or a bug in Envoy's own header forwarding, would look
/// like. The ambiguity must resolve toward the scanning path, not
/// toward treating an unrecognised shape as safe to stream through.
#[test]
fn ambiguous_content_type_defaults_to_buffered_not_sse_passthrough() {
    let e = engine();
    let m = metrics();
    let mut state = ResponseState::new(64);
    let body =
        r#"{"choices":[{"message":{"content":"token ghp_abcdefghijklmnopqrstuvwxyz0123456789"}}]}"#;
    let resp = handle_response_chunk(&e, &m, &mut state, body.as_bytes(), true);
    assert!(
        matches!(&resp.response, Some(Resp::ImmediateResponse(_))),
        "an unlabelled response must still be scanned and blocked, got {resp:?}"
    );
}

#[test]
fn non_streaming_response_body_split_across_chunks_is_still_scanned_whole() {
    let e = engine();
    let m = metrics();
    let mut state = response_state_with_content_type(64, "application/json");
    let body =
        r#"{"choices":[{"message":{"content":"token ghp_abcdefghijklmnopqrstuvwxyz0123456789"}}]}"#;
    let (chunk1, chunk2) = body.as_bytes().split_at(body.len() / 2);

    let resp1 = handle_response_chunk(&e, &m, &mut state, chunk1, false);
    assert!(
        !matches!(&resp1.response, Some(Resp::ImmediateResponse(_))),
        "must not decide anything before end_of_stream: {resp1:?}"
    );
    assert_eq!(
        extract_body(&resp1),
        Some(Vec::new()),
        "nothing releases before the whole body has been scanned"
    );

    let resp2 = handle_response_chunk(&e, &m, &mut state, chunk2, true);
    assert!(
        matches!(&resp2.response, Some(Resp::ImmediateResponse(_))),
        "a credential split across response chunks must still block, got {resp2:?}"
    );
}
