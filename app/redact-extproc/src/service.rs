//! The `ExternalProcessor` gRPC service.
//!
//! Two directions, two shapes, matching ADR-0116's split:
//!
//! - **Request** — walked once the whole JSON body is available (the
//!   `EnvoyExtensionPolicy` sets `processingMode.request.body: Buffered`, so
//!   Envoy hands us exactly one `RequestBody` message covering the whole
//!   payload). Identical logic to `redact-gateway`'s request path, since the
//!   input shape is identical.
//! - **Response** — Envoy's `processingMode.response.body: Streamed` sends
//!   *every* response through this path, not only genuine SSE completions:
//!   `stream: false` completions and embeddings responses arrive the same
//!   way, in chunks. Which of the two shapes a given response actually has
//!   is resolved from the upstream `Content-Type` header (see
//!   [`ResponseState::set_mode_from_headers`]) into one of two handling
//!   modes:
//!   - **SSE** (`Content-Type: text/event-stream`): scanned incrementally via
//!     [`governance_redact::SseHoldBack`] as chunks arrive, so output lags
//!     input by a bounded window rather than by the length of the
//!     completion. `SseHoldBack` is frame-aware: it extracts `delta.content`
//!     and every tool call's `function.arguments` before redacting anything
//!     (the same rule [`governance_redact::scan_sse`]'s buffered path uses)
//!     and snaps every release to a whole SSE frame boundary, so a redaction
//!     operator's replacement can never land partway through a frame's JSON
//!     — the front-proxy-era limitation this module used to carry (a
//!     raw-byte [`governance_redact::HoldBack`] with no notion of SSE
//!     structure) is closed.
//!   - **Buffered** (anything else, including a missing or unrecognised
//!     Content-Type): accumulated in full and scanned in one pass at
//!     `end_of_stream`, mirroring `redact-gateway`'s non-streaming response
//!     path. This is the fail-closed default — `SseHoldBack` only ever
//!     examines `data:` lines, so feeding it a plain JSON body (because SSE
//!     was wrongly assumed) would release every byte as
//!     `Frame::Passthrough` with zero calls to `engine.scan`. That was a
//!     real gap: prior to this mode existing, every non-SSE response body
//!     ext_proc's `Streamed` setting handed us went out completely
//!     unscanned. An ambiguous Content-Type buffers rather than streams —
//!     "unknown" routes to the branch that inspects the whole body before
//!     releasing anything, not to the one that assumes it is safe to
//!     stream through.
//!
//! A response chunk boundary landing mid-UTF-8 codepoint (SSE mode only —
//! the buffered mode hands raw bytes straight to `serde_json`, which does
//! its own UTF-8 validation over the complete body) is handled by carrying
//! the incomplete trailing bytes over to the next chunk (see
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
        BodyMutation, BodyResponse, CommonResponse, HeaderMutation, HeadersResponse, HttpHeaders,
        ImmediateResponse, ProcessingRequest, ProcessingResponse, body_mutation,
        common_response::ResponseStatus, external_processor_server::ExternalProcessor,
        processing_request::Request as Req, processing_response::Response as Resp,
    },
    r#type::v3::{HttpStatus, StatusCode},
};
use governance_redact::{Engine, ScanReport, SseEmit, SseHoldBack, scan_request, scan_response};
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

/// Which shape a response body actually has, resolved from the upstream
/// `Content-Type` header. See the module doc for why the default (set in
/// [`ResponseState::new`]) is [`Self::Buffered`], not [`Self::Sse`].
enum ResponseBodyMode {
    /// `Content-Type: text/event-stream`. Handled incrementally via
    /// [`SseHoldBack`].
    Sse,
    /// Everything else. The accumulated raw bytes, scanned as one JSON body
    /// at `end_of_stream` — see [`handle_buffered_response_chunk`].
    Buffered(Vec<u8>),
}

/// State threaded across every `ResponseBody` chunk of one HTTP exchange.
struct ResponseState {
    hold: Box<SseHoldBack>,
    /// Redactions reported as of the last chunk, so the cumulative counter
    /// `SseHoldBack::redactions` can be turned into a per-chunk delta for
    /// the Prometheus counter. Only advances in [`ResponseBodyMode::Sse`].
    last_redactions: usize,
    /// Trailing bytes from the previous chunk that did not form a complete
    /// UTF-8 codepoint on their own. See [`decode_chunk_with_carry`]. Only
    /// used in [`ResponseBodyMode::Sse`].
    utf8_carry: Vec<u8>,
    mode: ResponseBodyMode,
    /// Header VALUES only (never a body), captured purely for diagnostics on
    /// a `handle_buffered_response_chunk` JSON-parse failure -- see that
    /// function's own comment on why a body snippet must never be logged
    /// (AGENTS.md: never log a request/response body) even though these two
    /// headers alone are usually enough to tell "this was compressed" from
    /// "this genuinely isn't JSON" apart.
    content_type: Option<String>,
    content_encoding: Option<String>,
}

impl ResponseState {
    fn new(window: usize) -> Self {
        Self {
            hold: Box::new(SseHoldBack::with_window(window)),
            last_redactions: 0,
            utf8_carry: Vec::new(),
            // Safe default until (or unless) the response headers say
            // otherwise — see the module doc's "Buffered" bullet.
            mode: ResponseBodyMode::Buffered(Vec::new()),
            content_type: None,
            content_encoding: None,
        }
    }

    /// Resolves [`Self::mode`] from the upstream response headers. Only an
    /// explicit `text/event-stream` `Content-Type` selects
    /// [`ResponseBodyMode::Sse`]; a missing header, or any other value,
    /// leaves the [`ResponseBodyMode::Buffered`] default from [`Self::new`]
    /// in place. Also captures `Content-Type`/`Content-Encoding` verbatim
    /// into [`Self::content_type`]/[`Self::content_encoding`] regardless of
    /// which mode is selected -- see those fields' own doc.
    ///
    /// Header keys arrive lower-cased already (Envoy's guarantee, see
    /// `HttpHeaders::headers`'s doc), but the value is matched
    /// case-insensitively and by prefix (`; charset=utf-8` and similar
    /// parameters are common) rather than relying on that.
    fn set_mode_from_headers(&mut self, headers: &HttpHeaders) {
        let Some(hm) = headers.headers.as_ref() else {
            return;
        };
        let hdr_val = |h: &HeaderValue| -> String {
            if !h.value.is_empty() {
                h.value.clone()
            } else {
                String::from_utf8_lossy(&h.raw_value).into_owned()
            }
        };
        let is_sse = hm.headers.iter().any(|h| {
            h.key.eq_ignore_ascii_case("content-type")
                && hdr_val(h)
                    .to_ascii_lowercase()
                    .starts_with("text/event-stream")
        });
        if is_sse {
            self.mode = ResponseBodyMode::Sse;
        }
        for h in &hm.headers {
            let val = hdr_val(h);
            if h.key.eq_ignore_ascii_case("content-type") {
                self.content_type = Some(val);
            } else if h.key.eq_ignore_ascii_case("content-encoding") {
                self.content_encoding = Some(val);
            }
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
            // DIAGNOSTIC: log response headers to verify Envoy sends them.
            // HeaderValue can carry value in `value` (string) or `raw_value`
            // (bytes). EG-generated envoy_grpc config populates both. Check
            // both to be safe across Envoy versions.
            if let Some(hm) = &headers.headers {
                for h in &hm.headers {
                    let val = if !h.value.is_empty() {
                        &h.value
                    } else {
                        std::str::from_utf8(&h.raw_value).unwrap_or("(empty)")
                    };
                    tracing::info!(key = %h.key, value = %val, "ResponseHeader");
                }
            } else {
                tracing::info!("ResponseHeaders received but headers field is None");
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
    let original_length = raw.len();
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
            // Pass original_length so body_response can update Content-Length
            // if the body was redacted (length changed).
            body_response_with_original_length(
                Direction::Request,
                serde_json::to_vec(&json).unwrap_or_else(|_| raw.to_vec()),
                Some(original_length),
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

/// Dispatches one response chunk to whichever handling mode
/// [`ResponseState::set_mode_from_headers`] resolved for this exchange.
///
/// See the module doc for why a response is not assumed to be SSE just
/// because it arrived through `processingMode.response.body: Streamed`.
fn handle_response_chunk(
    engine: &Engine,
    metrics: &Metrics,
    state: &mut ResponseState,
    chunk: &[u8],
    end_of_stream: bool,
) -> ProcessingResponse {
    if let ResponseBodyMode::Buffered(buf) = &mut state.mode {
        return handle_buffered_response_chunk(
            engine,
            metrics,
            buf,
            chunk,
            end_of_stream,
            state.content_type.as_deref(),
            state.content_encoding.as_deref(),
        );
    }

    let ResponseState {
        hold,
        last_redactions,
        utf8_carry,
        ..
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

/// Ceiling on a buffered (non-SSE) response body, mirroring
/// `redact-gateway`'s `read_capped` cap. `SseHoldBack` bounds its own memory
/// to the hold-back window regardless of stream length, but the buffered
/// path accumulates the whole body before it can be scanned — the same
/// trade `redact-gateway::read_capped`'s doc explains — so without a
/// ceiling a provider that never sets `Content-Type: text/event-stream` but
/// streams without stopping would grow this buffer until the pod is
/// OOM-killed.
const MAX_BUFFERED_RESPONSE_BYTES: usize = 33_554_432;

/// Accumulates a non-SSE response body — a plain JSON completion or
/// embeddings response, or anything whose `Content-Type` was not
/// `text/event-stream` — and scans it in one pass at `end_of_stream`, the
/// same way `redact-gateway`'s buffered response path (`scan_response`)
/// does. Nothing is released to the client before then: every non-final
/// chunk answers with an empty `body_mutation`, and the whole redacted body
/// is attached to the final one. `SseHoldBack`'s frame-by-frame release
/// cannot be reused here — it only ever looks inside `data:` lines, and a
/// plain JSON body has none, which is exactly the gap this function closes.
fn handle_buffered_response_chunk(
    engine: &Engine,
    metrics: &Metrics,
    buf: &mut Vec<u8>,
    chunk: &[u8],
    end_of_stream: bool,
    content_type: Option<&str>,
    content_encoding: Option<&str>,
) -> ProcessingResponse {
    if buf.len().saturating_add(chunk.len()) > MAX_BUFFERED_RESPONSE_BYTES {
        tracing::warn!(
            max_bytes = MAX_BUFFERED_RESPONSE_BYTES,
            "buffered response exceeded size cap"
        );
        return refuse_or_block(
            Direction::Response,
            engine,
            metrics,
            &format!("response exceeded {MAX_BUFFERED_RESPONSE_BYTES} bytes"),
        );
    }
    buf.extend_from_slice(chunk);

    if !end_of_stream {
        // Nothing is safe to release until the whole body has been scanned.
        return body_response(Direction::Response, Vec::new());
    }

    let mut json = match serde_json::from_slice::<serde_json::Value>(buf) {
        Ok(json) => json,
        Err(e) => {
            // Diagnostics only, deliberately narrow: header VALUES, byte
            // count, a gzip-magic-byte check, and serde_json's own error
            // (position/expected-token, never the offending bytes) -- never
            // the body itself, or even a snippet of it. AGENTS.md is
            // explicit that a request/response body is never logged, and
            // this is exactly the component that exists to keep PII/secrets
            // in a response from leaking anywhere they shouldn't -- logging
            // a "sample" of the very body this filter couldn't clear would
            // defeat its own purpose. This is deliberately enough to
            // distinguish "the response was compressed and we're trying to
            // parse ciphertext-looking bytes as JSON" from "the response
            // genuinely isn't JSON" without ever needing the content itself.
            tracing::error!(
                content_type,
                content_encoding,
                body_len = buf.len(),
                looks_gzip = buf.starts_with(&[0x1f, 0x8b]),
                parse_error = %e,
                "buffered response body did not parse as JSON"
            );
            return refuse_or_block(
                Direction::Response,
                engine,
                metrics,
                "response body is not JSON",
            );
        }
    };

    match scan_response(engine, &mut json) {
        Ok(report) => {
            record(metrics, &report);
            if report.is_blocked() {
                metrics.blocked_total.inc();
                tracing::warn!(entities = ?report.blocked, "blocked response: prohibited content");
                return immediate_response(
                    Direction::Response,
                    StatusCode::UnprocessableEntity,
                    "content_blocked",
                    &format!(
                        "response blocked: content matched a prohibited category ({})",
                        report.blocked.join(", ")
                    ),
                );
            }
            body_response_with_original_length(
                Direction::Response,
                serde_json::to_vec(&json).unwrap_or_else(|_| buf.clone()),
                Some(buf.len()),
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
    body_response_with_original_length(dir, bytes, None)
}

/// Builds a body response with optional Content-Length header mutation.
///
/// When `original_length` is `Some(old_len)` and `bytes.len()` (the new length)
/// differs from `old_len`, this includes a HeaderMutation that **removes** the
/// Content-Length header. Envoy v1.32 strips Content-Length to an empty value
/// when a body mutation changes the length, and an empty Content-Length causes
/// HTTP/2 upstreams to RST_STREAM with PROTOCOL_ERROR and HTTP/1.1 upstreams to
/// misframe the body. Removing the header entirely lets Envoy frame the
/// mutated body correctly: chunked transfer encoding over HTTP/1.1, DATA frames
/// over HTTP/2 — neither of which needs Content-Length.
///
/// Setting Content-Length to the new value (via `OverwriteIfExistsOrAdd`) was
/// the first attempt, but Envoy v1.32's ext_proc filter empties it *after*
/// applying the header mutation, so the overwrite never reaches the wire. The
/// `allow_content_length_header` config field that would prevent this was
/// added in Envoy v1.33+ and does not exist in v1.32.
fn body_response_with_original_length(
    dir: Direction,
    bytes: Vec<u8>,
    original_length: Option<usize>,
) -> ProcessingResponse {
    let header_mutation = original_length.and_then(|old_len| {
        if bytes.len() == old_len {
            return None;
        }
        Some(HeaderMutation {
            set_headers: Vec::new(),
            remove_headers: vec!["content-length".into()],
        })
    });

    let common = CommonResponse {
        status: ResponseStatus::Continue as i32,
        header_mutation,
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
        let body = r#"{"choices":[{"message":{"content":"here: ghp_abcdefghijklmnopqrstuvwxyz0123456789"}}]}"#;
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
        let body = r#"{"choices":[{"message":{"content":"token ghp_abcdefghijklmnopqrstuvwxyz0123456789"}}]}"#;
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
        let body = r#"{"choices":[{"message":{"content":"token ghp_abcdefghijklmnopqrstuvwxyz0123456789"}}]}"#;
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
}
