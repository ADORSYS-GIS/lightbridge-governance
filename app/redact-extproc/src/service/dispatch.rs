//! The `ExternalProcessor` gRPC message loop and the per-message state
//! machine that drives the per-phase handlers.
//!
//! Split out of `service.rs` (issue #180) along the "dispatch" seam: the
//! `process` stream loop plus the `dispatch` state machine that routes each
//! inbound message to a phase handler. The handlers themselves live in
//! [`super::request`] and [`super::response`]; the state they mutate lives
//! in [`super::state`].

use std::sync::Arc;

use envoy_types::pb::envoy::{
    service::ext_proc::v3::{
        CommonResponse, HeadersResponse, ProcessingRequest, ProcessingResponse,
        common_response::ResponseStatus, external_processor_server::ExternalProcessor,
        processing_request::Request as Req, processing_response::Response as Resp,
    },
    r#type::v3::StatusCode,
};
use governance_redact::Engine;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use super::{
    Direction, Phase, RedactProcessor, ResponseState, handle_request_body, handle_response_chunk,
    immediate_response,
};
use crate::metrics::Metrics;

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
pub(crate) fn dispatch(
    req: Req,
    phase: &mut Phase,
    engine: &Engine,
    metrics: &Metrics,
    window: usize,
) -> Option<ProcessingResponse> {
    match (req, &mut *phase) {
        (Req::RequestHeaders(_), _) => {
            // Accept-Encoding is left intact — the upstream decides how to
            // encode the response. For `stream: false` (JSON) this upstream
            // returns gzip when the header is present, which we decompress
            // before scanning (see `decompress_gzip`). For `stream: true`
            // (SSE) it returns plaintext regardless of the header, which the
            // incremental `SseHoldBack` path handles directly. Either way
            // the response-path code (`handle_response_chunk`) resolves the
            // correct handling from the response headers (Content-Type and
            // Content-Encoding), not from what we sent in the request.
            Some(continue_headers(Direction::Request))
        }

        (Req::ResponseHeaders(headers), phase) => {
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
            // Resolves SSE-vs-buffered before any `ResponseBody` chunk
            // arrives (Envoy always sends headers first) -- see
            // `ResponseState::set_mode_from_headers`. Reachable for both the
            // normal case (phase was already `ResponseBody`) and the
            // bodyless case just above (phase just became `ResponseBody`):
            // dropping this call here would silently default every response
            // to `Buffered` mode, which the SSE-vs-buffered module doc and
            // this file's own SSE integration tests exist to hold in place.
            if let Phase::ResponseBody(state) = phase {
                state.set_mode_from_headers(&headers);
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

pub(crate) fn continue_headers(dir: Direction) -> ProcessingResponse {
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
