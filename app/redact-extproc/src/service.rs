//! The `ExternalProcessor` gRPC service.
//!
//! Two directions, two shapes, matching ADR-0116's split:
//!
//! - **Request** — walked once the whole JSON body is available (the
//!   `EnvoyExtensionPolicy` sets `processingMode.request.body: Buffered`, so
//!   Envoy hands us exactly one `RequestBody` message covering the whole
//!   payload). Identical logic to `redact-gateway`'s request path, since the
//!   input shape is identical.
//! - **Response** — scanned incrementally via
//!   [`governance_redact::SseHoldBack`] as chunks arrive under
//!   `processingMode.response.body: Streamed`, so output lags input by a
//!   bounded window rather than by the length of the completion.
//!   `SseHoldBack` is frame-aware: it extracts exactly `delta.content` before
//!   redacting anything (the same rule
//!   [`governance_redact::scan_sse`]'s buffered path uses) and snaps every
//!   release to a whole SSE frame boundary, so a redaction operator's
//!   replacement can never land partway through a frame's JSON — the
//!   front-proxy-era limitation this module used to carry (a raw-byte
//!   [`governance_redact::HoldBack`] with no notion of SSE structure) is
//!   closed.
//!
//! A response chunk boundary landing mid-UTF-8 codepoint is handled by
//! carrying the incomplete trailing bytes over to the next chunk (see
//! [`decode_chunk_with_carry`]) rather than failing the request — this is
//! a routine consequence of chunked delivery, not evidence of anything
//! wrong with the content, and treating it as an error broke nearly every
//! short completion in production (2026-08-03): upstream framing put a
//! multi-byte character at a fixed offset that split on almost every
//! reply, not on some rare unlucky one.

use std::sync::Arc;

use envoy_types::pb::envoy::{
    config::core::v3::{HeaderValue, HeaderValueOption},
    service::ext_proc::v3::{
        BodyMutation, BodyResponse, CommonResponse, HeaderMutation, HeadersResponse,
        ImmediateResponse, ProcessingRequest, ProcessingResponse, body_mutation,
        common_response::ResponseStatus, external_processor_server::ExternalProcessor,
        processing_request::Request as Req, processing_response::Response as Resp,
    },
    r#type::v3::{HttpStatus, StatusCode},
};
use governance_redact::{Engine, ScanReport, SseEmit, SseHoldBack, scan_request};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use crate::metrics::Metrics;

/// Implements Envoy's `ExternalProcessor` service against a shared
/// [`Engine`].
pub struct RedactProcessor {
    engine: Arc<Engine>,
    metrics: Arc<Metrics>,
    response_window: usize,
}

impl RedactProcessor {
    #[must_use]
    pub const fn new(engine: Arc<Engine>, metrics: Arc<Metrics>, response_window: usize) -> Self {
        Self {
            engine,
            metrics,
            response_window,
        }
    }
}

/// Which body direction a `CommonResponse` answers. Envoy's
/// `ProcessingResponse.response` oneof has a distinct variant per direction
/// (`RequestBody` vs `ResponseBody`) even though the payload shape
/// (`BodyResponse`) is identical — wrapping in the wrong one is accepted by
/// the type system (both are `BodyResponse`) but answers the wrong message
/// on Envoy's side of the stream.
#[derive(Clone, Copy)]
enum Direction {
    Request,
    Response,
}

/// Per-stream state. One `process` call is one HTTP request/response pair;
/// nothing here is shared across calls.
enum Phase {
    /// Accumulating the request body. `Buffered` mode means this holds the
    /// whole payload by the time `end_of_stream` is set, but chunks are
    /// concatenated defensively rather than assuming exactly one message.
    RequestBody(Vec<u8>),
    /// Request handling is done; now accumulating the streamed response.
    ResponseBody(ResponseState),
}

/// State threaded across every `ResponseBody` chunk of one HTTP exchange.
struct ResponseState {
    hold: Box<SseHoldBack>,
    /// Redactions reported as of the last chunk, so the cumulative counter
    /// `SseHoldBack::redactions` can be turned into a per-chunk delta for
    /// the Prometheus counter.
    last_redactions: usize,
    /// Trailing bytes from the previous chunk that did not form a complete
    /// UTF-8 codepoint on their own. See [`decode_chunk_with_carry`].
    utf8_carry: Vec<u8>,
}

impl ResponseState {
    fn new(window: usize) -> Self {
        Self {
            hold: Box::new(SseHoldBack::with_window(window)),
            last_redactions: 0,
            utf8_carry: Vec::new(),
        }
    }
}

#[tonic::async_trait]
impl ExternalProcessor for RedactProcessor {
    type ProcessStream = ReceiverStream<Result<ProcessingResponse, Status>>;

    async fn process(
        &self,
        request: Request<Streaming<ProcessingRequest>>,
    ) -> Result<Response<Self::ProcessStream>, Status> {
        let mut inbound = request.into_inner();
        let (tx, rx) = mpsc::channel(4);

        let engine = Arc::clone(&self.engine);
        let metrics = Arc::clone(&self.metrics);
        let window = self.response_window;

        tokio::spawn(async move {
            let mut phase = Phase::RequestBody(Vec::new());

            loop {
                let msg = match inbound.message().await {
                    Ok(Some(m)) => m,
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(error = %e, "ext_proc stream read failed");
                        break;
                    }
                };

                let Some(req) = msg.request else { continue };

                let Some(out) = dispatch(req, &mut phase, &engine, &metrics, window) else {
                    continue; // waiting on more chunks of a buffered body
                };

                let should_stop = matches!(&out.response, Some(Resp::ImmediateResponse(_)));
                if tx.send(Ok(out)).await.is_err() {
                    break; // client disconnected
                }
                if should_stop {
                    break;
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

/// Advances `phase` per the inbound message and decides what to tell Envoy.
/// `None` means "wait for more chunks", not an answer to send.
///
/// Split out of `process()`'s loop so the state machine can be driven
/// directly by a test, not only through a live gRPC stream.
fn dispatch(
    req: Req,
    phase: &mut Phase,
    engine: &Engine,
    metrics: &Metrics,
    window: usize,
) -> Option<ProcessingResponse> {
    match (req, &mut *phase) {
        (Req::RequestHeaders(_), _) => Some(continue_headers(Direction::Request)),

        (Req::ResponseHeaders(_), phase) => {
            // A bodyless request (GET, health checks, ...) never gets a
            // RequestBody message at all -- Envoy signals "no body" via
            // RequestHeaders.end_of_stream instead. Without this, `phase`
            // stays stuck at RequestBody, the ResponseBody message that
            // follows can't match any other arm, and it falls into the
            // catch-all below: an unconditional 500 on every bodyless
            // request. Reproduced live 2026-08-06 ("processing state
            // mismatch" on GET /v1/models) -- this filter attaches
            // Gateway-wide, so that's not a rare path.
            if matches!(phase, Phase::RequestBody(_)) {
                *phase = Phase::ResponseBody(ResponseState::new(window));
            }
            Some(continue_headers(Direction::Response))
        }

        (Req::RequestBody(body), Phase::RequestBody(buf)) => {
            buf.extend_from_slice(&body.body);
            if !body.end_of_stream {
                // Buffered mode should not send a partial chunk, but if it
                // ever does, wait for the rest rather than scanning an
                // incomplete JSON body.
                return None;
            }
            metrics.requests_total.inc();
            let result = handle_request_body(engine, metrics, buf);
            *phase = Phase::ResponseBody(ResponseState::new(window));
            Some(result)
        }

        (Req::ResponseBody(body), Phase::ResponseBody(state)) => Some(handle_response_chunk(
            engine,
            metrics,
            state,
            &body.body,
            body.end_of_stream,
        )),

        // A body message arrived in a phase nothing above expected (e.g. a
        // ResponseBody before the request finished, or -- until the arm
        // above -- a ResponseBody with no preceding RequestBody at all).
        // A fail-closed component does not get to assume its own
        // invariants hold — refuse rather than guess which direction to
        // answer in. Logged, unlike before: this branch produced zero
        // log output during the 2026-08-06 incident, which is why it took
        // a live repro instead of the logs to find.
        _ => {
            tracing::warn!(
                "ext_proc processing state mismatch (unexpected message for the current phase)"
            );
            Some(immediate_response(
                Direction::Request,
                StatusCode::InternalServerError,
                "internal_error",
                "processing state mismatch",
            ))
        }
    }
}

fn continue_headers(dir: Direction) -> ProcessingResponse {
    let common = CommonResponse {
        status: ResponseStatus::Continue as i32,
        ..Default::default()
    };
    let response = match dir {
        Direction::Request => Resp::RequestHeaders(HeadersResponse {
            response: Some(common),
        }),
        Direction::Response => Resp::ResponseHeaders(HeadersResponse {
            response: Some(common),
        }),
    };
    ProcessingResponse {
        response: Some(response),
        ..Default::default()
    }
}

/// Scans the whole (buffered) request body and decides what to tell Envoy.
fn handle_request_body(engine: &Engine, metrics: &Metrics, raw: &[u8]) -> ProcessingResponse {
    let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(raw) else {
        return refuse_or_block(
            Direction::Request,
            engine,
            metrics,
            "request body is not JSON",
        );
    };

    match scan_request(engine, &mut json) {
        Ok(report) => {
            record(metrics, &report);
            if report.is_blocked() {
                metrics.blocked_total.inc();
                tracing::warn!(entities = ?report.blocked, "blocked request: prohibited content");
                return immediate_response(
                    Direction::Request,
                    StatusCode::UnprocessableEntity,
                    "content_blocked",
                    &format!(
                        "request blocked: content matched a prohibited category ({})",
                        report.blocked.join(", ")
                    ),
                );
            }
            if report.scanned_fields == 0 {
                metrics.uninspected_total.inc();
                tracing::warn!(
                    "request body had no recognised text fields; forwarding uninspected"
                );
            }
            body_response(
                Direction::Request,
                serde_json::to_vec(&json).unwrap_or_else(|_| raw.to_vec()),
            )
        }
        Err(e) => refuse_or_block(
            Direction::Request,
            engine,
            metrics,
            &format!("request scan failed: {e}"),
        ),
    }
}

/// Decodes as much valid UTF-8 as possible from `carry` followed by `chunk`,
/// leaving any trailing incomplete codepoint in `carry` for the next call.
///
/// A chunk boundary landing mid-codepoint is a routine consequence of
/// chunked delivery — nothing about the content is wrong, only about where
/// the transport happened to cut it — so carrying the tail forward is the
/// fix, not an error to raise. Only genuinely malformed UTF-8 (a byte
/// sequence no valid codepoint could ever complete) reaches the caller as
/// an error.
///
/// # Errors
///
/// Returns an error if the bytes preceding the incomplete tail are not
/// themselves valid UTF-8 — i.e. corruption, not just a split boundary.
fn decode_chunk_with_carry(carry: &mut Vec<u8>, chunk: &[u8]) -> Result<String, ()> {
    carry.extend_from_slice(chunk);
    match std::str::from_utf8(carry) {
        Ok(s) => {
            let s = s.to_string();
            carry.clear();
            Ok(s)
        }
        Err(e) => {
            // The distinction that matters: `error_len()` is `Some(_)` for a
            // byte sequence that is invalid NOW, and would stay invalid no
            // matter what bytes arrive after it (e.g. 0xFF is never a legal
            // lead byte) — that is corruption. It is `None` specifically
            // when the buffer ends partway through what could still become
            // a valid codepoint once more bytes arrive — that is an
            // ordinary chunk-boundary split. `valid_up_to()` alone cannot
            // tell these apart: it silently returned `0` for a definitely-bad
            // leading byte in an earlier version of this function, which
            // carried the bad byte forward forever instead of erroring.
            if e.error_len().is_some() {
                return Err(());
            }
            let valid_up_to = e.valid_up_to();
            let s = carry
                .get(..valid_up_to)
                .and_then(|b| std::str::from_utf8(b).ok())
                .unwrap_or_default()
                .to_string();
            carry.drain(..valid_up_to);
            Ok(s)
        }
    }
}

/// Feeds one response chunk through the incremental redactor.
///
/// See the module doc for the known SSE-framing gap this does not yet
/// close.
fn handle_response_chunk(
    engine: &Engine,
    metrics: &Metrics,
    state: &mut ResponseState,
    chunk: &[u8],
    end_of_stream: bool,
) -> ProcessingResponse {
    let ResponseState {
        hold,
        last_redactions,
        utf8_carry,
    } = state;

    let Ok(text) = decode_chunk_with_carry(utf8_carry, chunk) else {
        return refuse_or_block(
            Direction::Response,
            engine,
            metrics,
            "response bytes are not valid UTF-8",
        );
    };

    // A non-empty carry at end-of-stream means the response ended mid
    // codepoint with no further bytes ever coming to complete it — that is
    // genuine truncation, not a chunk-boundary artifact, and must still
    // fail closed rather than silently drop the incomplete tail.
    if end_of_stream && !utf8_carry.is_empty() {
        return refuse_or_block(
            Direction::Response,
            engine,
            metrics,
            "response ended mid UTF-8 codepoint",
        );
    }

    let emit = if end_of_stream {
        hold.push(engine, &text).and_then(|first| {
            // `first` can only be `Nothing` or `Release` past this point —
            // a block short-circuits before `flush` runs, so the released
            // text so far is never silently discarded in favour of a block
            // that arrives from the SAME chunk it was derived from.
            let first = match first {
                SseEmit::Blocked(entities) => return Ok(SseEmit::Blocked(entities)),
                other => other,
            };
            let last = hold.flush(engine)?;
            Ok(match (first, last) {
                (SseEmit::Release(mut a), SseEmit::Release(b)) => {
                    a.push_str(&b);
                    SseEmit::Release(a)
                }
                (SseEmit::Release(a), SseEmit::Nothing) => SseEmit::Release(a),
                (SseEmit::Nothing, other) => other,
                (SseEmit::Blocked(_), _) => unreachable!("Blocked already returned above"),
                // `HoldBack::advance` checks the WHOLE pending buffer for a
                // block before computing any cut, in both `push` and
                // `flush`. Since `first` (this push) was not `Blocked`, no
                // blocking span exists anywhere in `pending` at that point —
                // and `safe_cut` never lets a span straddle the cut, so the
                // text `flush` sees afterward is a strict, span-clean
                // subset of what `push` already scanned clean. A block
                // surfacing only here would mean the same text was clean
                // moments ago and is not now, with nothing appended between
                // the two calls.
                (SseEmit::Release(_), SseEmit::Blocked(_)) => {
                    unreachable!("flush cannot find a block that push's scan of the same pending buffer did not")
                }
            })
        })
    } else {
        hold.push(engine, &text)
    };

    let delta = hold.redactions().saturating_sub(*last_redactions);
    if delta > 0 {
        metrics.redactions_total.inc_by(delta as u64);
        *last_redactions = hold.redactions();
    }

    match emit {
        Ok(SseEmit::Nothing) => body_response(Direction::Response, Vec::new()),
        Ok(SseEmit::Release(out)) => body_response(Direction::Response, out.into_bytes()),
        Ok(SseEmit::Blocked(entities)) => {
            metrics.blocked_total.inc();
            tracing::warn!(?entities, "blocked response: prohibited content");
            immediate_response(
                Direction::Response,
                StatusCode::UnprocessableEntity,
                "content_blocked",
                &format!(
                    "response blocked: content matched a prohibited category ({})",
                    entities.join(", ")
                ),
            )
        }
        Err(e) => refuse_or_block(
            Direction::Response,
            engine,
            metrics,
            &format!("response scan failed: {e}"),
        ),
    }
}

fn record(metrics: &Metrics, report: &ScanReport) {
    if report.redactions > 0 {
        metrics.redactions_total.inc_by(report.redactions as u64);
    }
    metrics
        .scanned_fields_total
        .inc_by(report.scanned_fields as u64);
}

/// Applies the profile's `fail_closed` rule to an indeterminate result.
fn refuse_or_block(
    dir: Direction,
    engine: &Engine,
    metrics: &Metrics,
    reason: &str,
) -> ProcessingResponse {
    if engine.profile().fail_closed {
        metrics.refused_total.inc();
        tracing::error!(reason, "failing closed");
        return immediate_response(
            dir,
            StatusCode::BadGateway,
            "redaction_unavailable",
            &format!("request refused: redaction could not be completed ({reason})"),
        );
    }
    metrics.fail_open_total.inc();
    tracing::warn!(
        reason,
        "redaction indeterminate on a non-fail-closed profile; continuing"
    );
    continue_headers(dir)
}

fn immediate_response(
    _dir: Direction,
    status: StatusCode,
    code: &str,
    message: &str,
) -> ProcessingResponse {
    // `ImmediateResponse` is direction-agnostic in the proto (it aborts and
    // replaces the whole HTTP exchange regardless of which body direction
    // triggered it), so `_dir` is accepted for symmetry with the other
    // helpers but unused here.
    let body = serde_json::json!({
        "error": { "message": message, "type": code, "code": code }
    })
    .to_string();

    ProcessingResponse {
        response: Some(Resp::ImmediateResponse(ImmediateResponse {
            status: Some(HttpStatus {
                code: status as i32,
            }),
            headers: Some(HeaderMutation {
                set_headers: vec![HeaderValueOption {
                    header: Some(HeaderValue {
                        key: "content-type".into(),
                        value: "application/json".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            body: body.into_bytes(),
            grpc_status: None,
            details: message.to_string(),
        })),
        ..Default::default()
    }
}

fn body_response(dir: Direction, bytes: Vec<u8>) -> ProcessingResponse {
    let common = CommonResponse {
        status: ResponseStatus::Continue as i32,
        body_mutation: Some(BodyMutation {
            mutation: Some(body_mutation::Mutation::Body(bytes)),
        }),
        ..Default::default()
    };
    let response = match dir {
        Direction::Request => Resp::RequestBody(BodyResponse {
            response: Some(common),
        }),
        Direction::Response => Resp::ResponseBody(BodyResponse {
            response: Some(common),
        }),
    };
    ProcessingResponse {
        response: Some(response),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use governance_redact::Profile;

    use super::*;

    fn engine() -> Engine {
        Engine::new(Profile::coding_assistant(), "test-salt").expect("engine")
    }

    fn metrics() -> Metrics {
        Metrics::new().expect("metrics")
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
        let mut state = ResponseState::new(64);

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
        let mut state = ResponseState::new(64);
        let resp = handle_response_chunk(&e, &m, &mut state, &[0xFF, 0xFE], true);
        assert!(
            matches!(&resp.response, Some(Resp::ImmediateResponse(_))),
            "genuinely malformed UTF-8 must still fail closed: {resp:?}"
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

        // Now the response body arrives, as it does for virtually every reply.
        let body = HttpBody {
            body: b"hello".to_vec(),
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
}
