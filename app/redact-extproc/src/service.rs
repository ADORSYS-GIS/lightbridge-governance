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
//! ⚠️ **Still not handled**: a response chunk boundary landing mid-UTF-8
//! codepoint. Real streamed non-ASCII text will hit this. Current behaviour
//! is to fail closed rather than silently misdecode — see
//! [`handle_response_chunk`].

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
    /// The `usize` is redactions reported as of the last chunk, so the
    /// cumulative counter `SseHoldBack::redactions` can be turned into a
    /// per-chunk delta for the Prometheus counter.
    ResponseBody(Box<SseHoldBack>, usize),
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

                let out = match (req, &mut phase) {
                    (Req::RequestHeaders(_), _) => continue_headers(Direction::Request),
                    (Req::ResponseHeaders(_), _) => continue_headers(Direction::Response),

                    (Req::RequestBody(body), Phase::RequestBody(buf)) => {
                        buf.extend_from_slice(&body.body);
                        if !body.end_of_stream {
                            // Buffered mode should not send a partial chunk,
                            // but if it ever does, wait for the rest rather
                            // than scanning an incomplete JSON body.
                            continue;
                        }
                        metrics.requests_total.inc();
                        let result = handle_request_body(&engine, &metrics, buf);
                        phase = Phase::ResponseBody(Box::new(SseHoldBack::with_window(window)), 0);
                        result
                    }

                    (Req::ResponseBody(body), Phase::ResponseBody(hold, last_redactions)) => {
                        handle_response_chunk(
                            &engine,
                            &metrics,
                            hold,
                            last_redactions,
                            &body.body,
                            body.end_of_stream,
                        )
                    }

                    // A body message arrived in the wrong phase (e.g. a
                    // ResponseBody before the request finished). Should be
                    // unreachable given Envoy's own message ordering, but a
                    // fail-closed component does not get to assume its own
                    // invariants — refuse rather than guess which direction
                    // to answer in.
                    _ => immediate_response(
                        Direction::Request,
                        StatusCode::InternalServerError,
                        "internal_error",
                        "processing state mismatch",
                    ),
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

/// Feeds one response chunk through the incremental redactor.
///
/// See the module doc for the known SSE-framing gap and the UTF-8 boundary
/// gap this does not yet close.
fn handle_response_chunk(
    engine: &Engine,
    metrics: &Metrics,
    hold: &mut SseHoldBack,
    last_redactions: &mut usize,
    chunk: &[u8],
    end_of_stream: bool,
) -> ProcessingResponse {
    let text = match std::str::from_utf8(chunk) {
        Ok(t) => t,
        Err(_) => {
            return refuse_or_block(
                Direction::Response,
                engine,
                metrics,
                "response chunk split a UTF-8 codepoint",
            );
        }
    };

    let emit = if end_of_stream {
        hold.push(engine, text).and_then(|first| {
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
        hold.push(engine, text)
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
