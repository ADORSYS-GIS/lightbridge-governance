//! Tests for the `dispatch` state machine — the bodyless-request and
//! Accept-Encoding behaviours (issue #180 split).

use envoy_types::pb::envoy::config::core::v3::HeaderMap;

use super::{super::*, engine, metrics};

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
