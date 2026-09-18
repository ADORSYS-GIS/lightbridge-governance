//! Tests for the reply builders — the Content-Length removal behaviour
//! (issue #180 split).

use super::super::*;

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
