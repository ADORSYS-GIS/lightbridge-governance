//! Tests for [`super`]. Split into its own file (issue #175) rather than
//! raising the already-grandfathered LoC ceiling -- the same move
//! `otel/tests.rs` made, applied to the whole `mod tests` block.

use envoy_types::pb::envoy::config::core::v3::HeaderMap;
use governance_redact::Profile;

use super::*;

fn engine() -> Engine {
    Engine::new(Profile::coding_assistant(), "test-salt").expect("engine")
}

fn metrics() -> Metrics {
    Metrics::new().expect("metrics")
}

/// Synthesizes the `ResponseHeaders` message Envoy sends before any
/// `ResponseBody` chunk, carrying one `Content-Type` value, and applies
/// it the way `process()`'s message loop does — so tests exercise the
/// real routing decision (`ResponseState::set_mode_from_headers`)
/// instead of relying on `ResponseState::new`'s default.
fn response_state_with_content_type(window: usize, content_type: &str) -> ResponseState {
    let mut state = ResponseState::new(window);
    state.set_mode_from_headers(&HttpHeaders {
        headers: Some(HeaderMap {
            headers: vec![HeaderValue {
                key: "content-type".to_string(),
                value: content_type.to_string(),
                raw_value: Vec::new(),
            }],
        }),
        attributes: std::collections::HashMap::new(),
        end_of_stream: false,
    });
    state
}

/// Like `response_state_with_content_type`, but also sets Content-Encoding
/// so the `is_sse && is_gzip` branch of `set_mode_from_headers` engages.
fn response_state_with_content_type_and_encoding(
    window: usize,
    content_type: &str,
    content_encoding: &str,
) -> ResponseState {
    let mut state = ResponseState::new(window);
    state.set_mode_from_headers(&HttpHeaders {
        headers: Some(HeaderMap {
            headers: vec![
                HeaderValue {
                    key: "content-type".to_string(),
                    value: content_type.to_string(),
                    raw_value: Vec::new(),
                },
                HeaderValue {
                    key: "content-encoding".to_string(),
                    value: content_encoding.to_string(),
                    raw_value: Vec::new(),
                },
            ],
        }),
        attributes: std::collections::HashMap::new(),
        end_of_stream: false,
    });
    state
}

fn extract_body(resp: &ProcessingResponse) -> Option<Vec<u8>> {
    let Some(Resp::ResponseBody(BodyResponse {
        response:
            Some(CommonResponse {
                body_mutation:
                    Some(BodyMutation {
                        mutation: Some(body_mutation::Mutation::Body(bytes)),
                    }),
                ..
            }),
    })) = &resp.response
    else {
        return None;
    };
    Some(bytes.clone())
}

#[test]
fn decode_chunk_with_carry_reassembles_a_split_codepoint() {
    // "é" = 0xC3 0xA9. Split across two calls so a lone leading byte is
    // carried, exactly what a fixed-size upstream frame boundary does.
    let mut carry = Vec::new();

    let first = decode_chunk_with_carry(&mut carry, b"caf").expect("ascii prefix");
    assert_eq!(first, "caf");
    assert!(carry.is_empty());

    let split = decode_chunk_with_carry(&mut carry, &[0xC3]).expect("lone leading byte");
    assert_eq!(
        split, "",
        "nothing releasable yet -- codepoint is incomplete"
    );
    assert_eq!(carry, vec![0xC3]);

    let rest = decode_chunk_with_carry(&mut carry, &[0xA9, b'!']).expect("completes it");
    assert_eq!(rest, "é!");
    assert!(carry.is_empty());
}

#[test]
fn decode_chunk_with_carry_rejects_genuinely_malformed_bytes() {
    let mut carry = Vec::new();
    // 0xFF is never a valid UTF-8 leading byte -- no continuation byte
    // could ever complete it, unlike a mere split boundary.
    assert!(decode_chunk_with_carry(&mut carry, &[0xFF, 0xFE]).is_err());
}

/// Reproduces the production incident directly (2026-08-03): an SSE
/// delta containing an em dash (3-byte UTF-8) split across chunk
/// boundaries at every possible cut point must still deliver the
/// content, not fail the request.
#[test]
fn split_codepoint_across_response_chunks_does_not_fail_the_request() {
    let e = engine();
    let m = metrics();
    // Content-Type is what routes this to the SSE path under test; a
    // response with no headers at all would take the safe Buffered
    // default instead (see the module doc), which is the wrong path for
    // this specific incident.
    let mut state = response_state_with_content_type(64, "text/event-stream");

    let frame =
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\u{2014}there\"}}]}\n\n";
    let bytes = frame.as_bytes();
    let dash_at = frame.find('\u{2014}').expect("frame contains the dash");
    assert_eq!("\u{2014}".len(), 3, "em dash must be 3 bytes for this test");

    // Cut so chunk 1 ends after the dash's first byte, chunk 2 is just
    // its second byte, chunk 3 carries the third byte plus the rest.
    let (chunk1, remainder) = bytes.split_at(dash_at + 1);
    let (chunk2, chunk3) = remainder.split_at(1);

    let mut collected = Vec::new();
    for (chunk, last) in [(chunk1, false), (chunk2, false), (chunk3, true)] {
        let resp = handle_response_chunk(&e, &m, &mut state, chunk, last);
        assert!(
            !matches!(&resp.response, Some(Resp::ImmediateResponse(_))),
            "request was rejected on a mere chunk-boundary split: {resp:?}"
        );
        if let Some(bytes) = extract_body(&resp) {
            collected.extend_from_slice(&bytes);
        }
    }

    let out = String::from_utf8(collected).expect("output is valid utf8");
    assert!(
        out.contains("hi\u{2014}there"),
        "content lost or mangled: {out}"
    );
}

/// The narrower failure mode this fix must still catch: bytes that are
/// not a chunk-boundary artifact but genuinely invalid, with no more
/// input ever coming to complete them.
#[test]
fn genuinely_malformed_final_chunk_still_fails_closed() {
    let e = engine();
    let m = metrics();
    // Routed via the SSE path specifically -- this is the UTF-8 carry
    // integration under test, not the Buffered path's independent
    // "not valid JSON" refusal (which would also fail closed here, but
    // for a different reason than the one this test names).
    let mut state = response_state_with_content_type(64, "text/event-stream");
    // opengrep-ignore: test vector, not a token
    let resp = handle_response_chunk(&e, &m, &mut state, &[0xFF, 0xFE], true);
    assert!(
        matches!(&resp.response, Some(Resp::ImmediateResponse(_))),
        "genuinely malformed UTF-8 must still fail closed: {resp:?}"
    );
}

// ── Non-SSE response bodies: the P0 this file used to miss entirely.
//    Every response chunk went into `SseHoldBack`, which only ever
//    looks inside `data:` lines -- a `stream: false` JSON completion
//    has none, so it sailed through as `Frame::Passthrough` with zero
//    calls to `engine.scan`. ─────────────────────────────────────────

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

/// Reproduces the 2026-08-06 production incident directly: Envoy signals
/// a bodyless request (GET, health checks, ...) via
/// `RequestHeaders.end_of_stream`, never sending a `RequestBody` message
/// at all. `phase` must advance to `ResponseBody` when `ResponseHeaders`
/// arrives with no body having come first -- otherwise the response body
/// that follows (virtually every response has one) can't match any arm
/// except the state-mismatch catch-all, and the request gets an
/// unconditional 500. This filter attaches Gateway-wide, so a bodyless
/// request is not a rare path -- it's every GET.
#[test]
fn bodyless_request_does_not_fail_the_response() {
    use envoy_types::pb::envoy::service::ext_proc::v3::{HttpBody, HttpHeaders};

    let e = engine();
    let m = metrics();
    let mut phase = Phase::RequestBody(Vec::new());

    let out = dispatch(
        Req::RequestHeaders(HttpHeaders::default()),
        &mut phase,
        &e,
        &m,
        64,
    );
    assert!(out.is_some(), "RequestHeaders must always get an answer");

    // No RequestBody message in between -- this is the bodyless case.
    let out = dispatch(
        Req::ResponseHeaders(HttpHeaders::default()),
        &mut phase,
        &e,
        &m,
        64,
    )
    .expect("ResponseHeaders must always get an answer");
    assert!(
        !matches!(&out.response, Some(Resp::ImmediateResponse(_))),
        "ResponseHeaders alone must never fail the exchange: {out:?}"
    );
    assert!(
        matches!(phase, Phase::ResponseBody(_)),
        "phase must advance past RequestBody once ResponseHeaders arrives with no RequestBody having come first"
    );

    // Now the response body arrives, as it does for virtually every
    // reply. Minimal valid JSON, not an arbitrary string: `ResponseHeaders`
    // carried no Content-Type here, so `phase` is now `Buffered` (the
    // module's own default, see `ResponseState::set_mode_from_headers`),
    // and Buffered mode requires the body to parse as JSON before it can
    // decide anything else -- a non-JSON stand-in would be refused for
    // that unrelated reason and this test would pass without ever
    // reaching the state-mismatch branch it exists to rule out.
    let body = HttpBody {
        body: b"{}".to_vec(),
        end_of_stream: true,
        ..Default::default()
    };
    let out = dispatch(Req::ResponseBody(body), &mut phase, &e, &m, 64)
        .expect("ResponseBody must always get an answer");
    assert!(
        !matches!(&out.response, Some(Resp::ImmediateResponse(_))),
        "response body must be handled normally, not rejected as a state mismatch: {out:?}"
    );
}

/// RequestHeaders no longer strips Accept-Encoding. The upstream receives
/// the client's original Accept-Encoding header and decides how to encode
/// the response. The response-path code resolves the correct handling from
/// Content-Type and Content-Encoding, not from request-time manipulation.
#[test]
fn request_headers_does_not_strip_accept_encoding() {
    use envoy_types::pb::envoy::service::ext_proc::v3::HttpHeaders;

    let e = engine();
    let m = metrics();
    let mut phase = Phase::RequestBody(Vec::new());

    let out = dispatch(
        Req::RequestHeaders(HttpHeaders::default()),
        &mut phase,
        &e,
        &m,
        64,
    )
    .expect("RequestHeaders must always get an answer");

    // The response must be a simple Continue without any header mutation
    // (no removal of Accept-Encoding from upstream-bound headers).
    let Some(Resp::RequestHeaders(HeadersResponse {
        response: Some(common),
    })) = &out.response
    else {
        panic!("expected RequestHeaders response, got {out:?}");
    };
    assert_eq!(
        common.status,
        ResponseStatus::Continue as i32,
        "RequestHeaders must Continue"
    );
    assert!(
        common.header_mutation.is_none(),
        "RequestHeaders must NOT mutate upstream headers (Accept-Encoding must be left intact): {common:?}"
    );
}

/// The bodyless-request fix above (`dispatch`'s `ResponseHeaders` arm)
/// transitions `phase` to `ResponseBody` itself, separately from the
/// pre-existing `ResponseHeaders` arm that resolves SSE-vs-buffered mode
/// (`ResponseState::set_mode_from_headers`). Both must run on the SAME
/// arrival of `ResponseHeaders` for the bodyless case: an SSE reply to a
/// bodyless request (any streamed completion behind a GET-triggered
/// redirect, for instance) must still resolve to `Sse` mode, not silently
/// fall back to `Buffered` because the mode-detection call is missing
/// from the branch that also does the phase transition.
#[test]
fn bodyless_request_sse_response_still_resolves_sse_mode() {
    use envoy_types::pb::envoy::service::ext_proc::v3::HttpHeaders;

    let e = engine();
    let m = metrics();
    let mut phase = Phase::RequestBody(Vec::new());

    dispatch(
        Req::RequestHeaders(HttpHeaders::default()),
        &mut phase,
        &e,
        &m,
        64,
    );

    // No RequestBody message in between -- the bodyless case -- and this
    // time ResponseHeaders carries an SSE Content-Type.
    let headers = HttpHeaders {
        headers: Some(HeaderMap {
            headers: vec![HeaderValue {
                key: "content-type".to_string(),
                value: "text/event-stream".to_string(),
                raw_value: Vec::new(),
            }],
        }),
        attributes: std::collections::HashMap::new(),
        end_of_stream: false,
    };
    dispatch(Req::ResponseHeaders(headers), &mut phase, &e, &m, 64)
        .expect("ResponseHeaders must always get an answer");

    let Phase::ResponseBody(state) = &phase else {
        panic!("phase must have advanced to ResponseBody");
    };
    assert!(
        matches!(state.mode, ResponseBodyMode::Sse),
        "an SSE Content-Type on the bodyless path must still resolve Sse mode, not silently default to Buffered"
    );
}

/// The gzip-SSE branch buffers compressed chunks, decompresses at
/// end_of_stream, scans through the SSE holdback, and re-compresses the
/// emitted output so the delivered body matches the still-present
/// `Content-Encoding: gzip` header. Without the re-compress step a
/// gzip-decoding client would receive identity bytes stamped as gzip.
#[test]
fn gzip_sse_is_decompressed_scanned_and_recompressed() {
    let e = engine();
    let m = metrics();
    let mut state = response_state_with_content_type_and_encoding(64, "text/event-stream", "gzip");

    // Assert the gzip-SSE mode actually engaged (gzip_buf armed).
    assert!(
        state.gzip_buf.is_some(),
        "SSE+gzip headers must arm the gzip buffer"
    );

    // Build gzip-compressed SSE: "data: hello from upstream\n\n".
    use std::io::Write;

    use flate2::{Compression, write::GzEncoder};
    let plaintext = b"data: hello from upstream\n\n";
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(plaintext).expect("write");
    let compressed = enc.finish().expect("finish");

    let resp = handle_response_chunk(&e, &m, &mut state, &compressed, true);
    let body = extract_body(&resp).expect("SSE gzip must emit a scanned body");

    // The emitted body must still be gzip-encoded (magic bytes) to match the
    // Content-Encoding: gzip header, NOT plaintext.
    assert!(
        body.starts_with(&[0x1f, 0x8b]),
        "gzip-SSE output must be re-compressed, got plaintext: {:?}",
        String::from_utf8_lossy(&body)
    );

    // Decompress it back and confirm the SSE line survived the round-trip.
    use std::io::Read;

    use flate2::read::GzDecoder;
    let mut dec = GzDecoder::new(&body[..]);
    let mut roundtrip = String::new();
    dec.read_to_string(&mut roundtrip).expect("decompress");
    assert_eq!(roundtrip, "data: hello from upstream\n\n");
}

/// A gzip decompression bomb (small compressed input that expands to far
/// more than `MAX_BUFFERED_RESPONSE_BYTES`) must be refused while
/// decompressing, not allocate gigabytes and only fail after the fact.
#[test]
fn gzip_decompression_bomb_is_refused_rather_than_oom() {
    use std::io::Write;

    use flate2::{Compression, write::GzEncoder};

    // Highly compressible input: >MAX bytes of 'x' compresses to far less
    // than MAX, but decompresses to a size that exceeds MAX_BUFFERED_RESPONSE_BYTES.
    const BOMB_SRC: usize = MAX_BUFFERED_RESPONSE_BYTES + (16 * 1024 * 1024);
    let mut enc = GzEncoder::new(Vec::new(), Compression::new(9));
    enc.write_all(&vec![b'x'; BOMB_SRC]).expect("write");
    let bomb = enc.finish().expect("finish");
    // Sanity: the compressed bomb must be much smaller than its intent
    // (otherwise the test isn't exercising the decompression cap).
    assert!(
        bomb.len() < MAX_BUFFERED_RESPONSE_BYTES,
        "bomb did not compress enough: {}",
        bomb.len()
    );

    let mut buf = bomb;
    let err = decompress_gzip(&mut buf).expect_err("decompression bomb must be refused");
    assert!(
        err.contains("exceeds"),
        "expected an 'exceeds' refusal, got: {err}"
    );
}

/// When a body mutation changes the body length, the Content-Length header
/// must be REMOVED, not overwritten. Envoy v1.32 strips Content-Length to
/// an empty value after applying a header mutation that sets it, and an
/// empty Content-Length causes HTTP/2 upstreams to RST_STREAM with
/// PROTOCOL_ERROR (reproduced live 2026-08-07: a request body redacted
/// 92 -> 86 bytes reached the upstream with Content-Length: "" and was
/// reset). Removing the header entirely lets Envoy frame the mutated body
/// via chunked encoding (HTTP/1.1) or DATA frames (HTTP/2), neither of
/// which requires Content-Length. The `allow_content_length_header` field
/// that would preserve a set Content-Length was added in Envoy v1.33+ and
/// does not exist in v1.32.
#[test]
fn content_length_removed_not_overwritten_when_length_changes() {
    // A mutation that shrinks the body 90 -> 84 bytes (as redaction does).
    let mut bytes = vec![b'x'; 84];
    bytes.extend_from_slice(b"tail");
    let resp = body_response_with_original_length(Direction::Request, bytes, Some(90));

    let Some(Resp::RequestBody(BodyResponse {
        response:
            Some(CommonResponse {
                header_mutation:
                    Some(HeaderMutation {
                        remove_headers,
                        set_headers,
                        ..
                    }),
                ..
            }),
    })) = &resp.response
    else {
        panic!("expected a RequestBody response with a header mutation, got {resp:?}");
    };

    assert!(
        remove_headers
            .iter()
            .any(|h| h.eq_ignore_ascii_case("content-length")),
        "Content-Length must be in remove_headers when the body length changed: {remove_headers:?}"
    );
    assert!(
        set_headers.is_empty(),
        "no headers should be set when the body length changed (removal only): {set_headers:?}"
    );
}

/// When the body length does NOT change, no Content-Length mutation is
/// needed — the original header is still correct.
#[test]
fn no_content_length_mutation_when_length_unchanged() {
    let bytes = vec![b'x'; 90];
    let resp = body_response_with_original_length(Direction::Request, bytes, Some(90));

    let Some(Resp::RequestBody(BodyResponse {
        response: Some(CommonResponse {
            header_mutation, ..
        }),
    })) = &resp.response
    else {
        panic!("expected a RequestBody response, got {resp:?}");
    };
    assert!(
        header_mutation.is_none(),
        "no header mutation expected when body length is unchanged"
    );
}
