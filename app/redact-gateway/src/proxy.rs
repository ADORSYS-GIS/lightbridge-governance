//! The proxy handler.
//!
//! One path in, one path out:
//!
//! ```text
//! client -> scan request -> upstream provider -> scan response -> client
//! ```
//!
//! # Fail-closed
//!
//! Every branch that cannot reach a confident "this content is safe to
//! forward" rejects the request. That includes the boring ones — a body that
//! will not parse as JSON, a scan that errors, an upstream response we cannot
//! read. On a `fail_closed` profile an outage of the detector is an outage of
//! the gateway, deliberately: the alternative is forwarding content nobody
//! inspected, which is the failure this service exists to prevent.
//!
//! `observe-only` is the one profile that does not fail closed, and it makes no
//! promises in exchange.

use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use governance_redact::{Engine, ScanReport, SseEmit, SseHoldBack, scan_request, scan_response};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::metrics::Metrics;

/// Shared handler state.
pub struct AppState {
    pub engine: Engine,
    pub client: reqwest::Client,
    pub provider_base_url: String,
    pub metrics: Metrics,
    /// Ceiling on an upstream response body. See [`read_capped`].
    pub max_body_bytes: usize,
}

/// Why a request was refused.
enum Refusal {
    /// Content must not leave. Carries entity TYPES only, never values.
    Blocked(Vec<String>),
    /// We could not determine whether the content was safe.
    Undetermined(String),
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Blocked(entities) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "content_blocked",
                format!(
                    "request blocked: content matched a prohibited category ({})",
                    entities.join(", ")
                ),
            ),
            Self::Undetermined(why) => (
                StatusCode::BAD_GATEWAY,
                "redaction_unavailable",
                format!("request refused: redaction could not be completed ({why})"),
            ),
        };

        // OpenAI-shaped error body, so a client's existing error handling works.
        let body = serde_json::json!({
            "error": { "message": message, "type": code, "code": code }
        });
        (status, axum::Json(body)).into_response()
    }
}

/// Streams an SSE response to the client chunk-by-chunk using hold-back
/// buffering.
///
/// Each `SseEmit::Release` is forwarded to the client as soon as the holdback
/// scanner clears it, so time-to-first-token equals the holdback window rather
/// than the full response duration. Memory per stream is bounded by
/// [`governance_redact::DEFAULT_WINDOW`] regardless of response size.
///
/// If a `Blocked` entity is detected mid-stream: when `fail_closed` is true,
/// a terminal OpenAI-shaped SSE error event is sent and the stream closes;
/// when false (observe-only) the content is forwarded and the block is logged.
///
/// On a non-UTF-8 chunk: a split codepoint is carried forward and retried;
/// a genuine corruption (invalid byte sequence) triggers the `fail_closed`
/// split the same way. Because HTTP headers are committed before scanning
/// begins, the status code stays 200; the in-band error payload signals the
/// block to OpenAI-compatible clients.
///
/// Metrics are recorded on both clean exit and each early-return path.
fn scan_sse_streaming(
    state: Arc<AppState>,
    status: StatusCode,
    upstream: reqwest::Response,
    fail_closed: bool,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, String>>(32);

    tokio::spawn(async move {
        let mut upstream = upstream;
        let mut holdback = SseHoldBack::with_window(governance_redact::DEFAULT_WINDOW);
        let mut total_bytes: usize = 0;
        let mut carry = Vec::new();
        let mut data_frames: usize = 0;
        let mut observe_passthrough = false;

        loop {
            let chunk = match upstream.chunk().await {
                Ok(Some(c)) => c,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(error = %e, "upstream read error during streaming scan");
                    if fail_closed {
                        let _ = tx
                            .send(Ok(Bytes::from(sse_error_event(
                                "redaction_unavailable",
                                &format!("upstream read error: {e}"),
                            ))))
                            .await;
                    }
                    state.metrics.record(&ScanReport {
                        redactions: holdback.redactions(),
                        scanned_fields: data_frames,
                        ..Default::default()
                    });
                    return;
                }
            };

            if observe_passthrough {
                if tx.send(Ok(chunk)).await.is_err() {
                    return;
                }
                continue;
            }

            total_bytes = total_bytes.saturating_add(chunk.len());
            if total_bytes > state.max_body_bytes {
                tracing::warn!(
                    max_bytes = state.max_body_bytes,
                    "upstream response exceeded size cap"
                );
                if fail_closed {
                    let _ = tx
                        .send(Ok(Bytes::from(sse_error_event(
                            "redaction_unavailable",
                            &format!("upstream response exceeded {} bytes", state.max_body_bytes),
                        ))))
                        .await;
                }
                state.metrics.record(&ScanReport {
                    redactions: holdback.redactions(),
                    scanned_fields: data_frames,
                    ..Default::default()
                });
                return;
            }

            let (text, incomplete_codepoint) = match decode_chunk_with_carry(&mut carry, &chunk) {
                Ok(r) => r,
                Err(()) => {
                    tracing::warn!("upstream response chunk is not valid UTF-8");
                    if fail_closed {
                        let _ = tx
                            .send(Ok(Bytes::from(sse_error_event(
                                "redaction_unavailable",
                                "upstream chunk is corrupt UTF-8",
                            ))))
                            .await;
                    }
                    state.metrics.record(&ScanReport {
                        redactions: holdback.redactions(),
                        scanned_fields: data_frames,
                        ..Default::default()
                    });
                    return;
                }
            };

            if incomplete_codepoint {
                tracing::trace!(
                    carry_len = carry.len(),
                    "UTF-8 codepoint split across chunks — carrying forward"
                );
                if !text.is_empty() {
                    let _ = holdback.push(&state.engine, &text).map_err(
                        |e| tracing::warn!(error = %e, "holdback emit after partial decode"),
                    );
                }
                continue;
            }

            data_frames += text
                .lines()
                .filter(|l| l.starts_with("data:") && *l != "data: [DONE]")
                .count();

            match holdback
                .push(&state.engine, &text)
                .map_err(|e| e.to_string())
            {
                Ok(SseEmit::Release(released)) => {
                    if tx.send(Ok(Bytes::from(released))).await.is_err() {
                        return; // client disconnected
                    }
                }
                Ok(SseEmit::Nothing) => {}
                Ok(SseEmit::Blocked(entities)) => {
                    tracing::warn!(
                        entities = ?entities,
                        "blocked response stream: prohibited content mid-stream"
                    );
                    state.metrics.blocked_total.inc();
                    if fail_closed {
                        let _ = tx.send(Ok(Bytes::from(sse_blocked_event(&entities)))).await;
                        state.metrics.record(&ScanReport {
                            redactions: holdback.redactions(),
                            scanned_fields: data_frames,
                            ..Default::default()
                        });
                        return;
                    }
                    state.metrics.record(&ScanReport {
                        redactions: holdback.redactions(),
                        scanned_fields: data_frames,
                        ..Default::default()
                    });
                    // observe-only: stop feeding holdback (it has latched); forward
                    // remaining chunks raw so the client sees the rest of the stream.
                    observe_passthrough = true;
                    // Fall through to next loop iteration where the passthrough guard
                    // above will kick in.
                }
                Err(e) => {
                    tracing::error!(error = %e, "response stream scan error");
                    if fail_closed {
                        let _ = tx
                            .send(Ok(Bytes::from(sse_error_event(
                                "redaction_unavailable",
                                &e,
                            ))))
                            .await;
                    }
                    state.metrics.record(&ScanReport {
                        redactions: holdback.redactions(),
                        scanned_fields: data_frames,
                        ..Default::default()
                    });
                    return;
                }
            }
        }

        // Flush any held text at end-of-stream.
        match holdback.flush(&state.engine).map_err(|e| e.to_string()) {
            Ok(SseEmit::Release(released)) => {
                let _ = tx.send(Ok(Bytes::from(released))).await;
            }
            Ok(SseEmit::Blocked(entities)) => {
                state.metrics.blocked_total.inc();
                let _ = tx.send(Ok(Bytes::from(sse_blocked_event(&entities)))).await;
            }
            Ok(SseEmit::Nothing) => {}
            Err(e) => {
                tracing::error!(error = %e, "response stream flush error");
                let _ = tx
                    .send(Ok(Bytes::from(sse_error_event(
                        "redaction_unavailable",
                        &e,
                    ))))
                    .await;
            }
        }

        state.metrics.record(&ScanReport {
            redactions: holdback.redactions(),
            scanned_fields: data_frames,
            ..Default::default()
        });
    });

    let body = Body::from_stream(ReceiverStream::new(rx));
    (status, [(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
}

/// Decodes as much valid UTF-8 as possible from `carry` followed by `chunk`,
/// leaving any trailing incomplete codepoint in `carry` for the next call.
///
/// A chunk boundary landing mid-codepoint is a routine consequence of chunked
/// delivery — nothing about the content is wrong, only where the transport
/// cut it — so carrying the tail forward is the fix, not an error to raise.
/// Only genuinely malformed UTF-8 (a byte sequence that no valid codepoint
/// could ever complete) reaches the caller as `Err(())`.
///
/// Returns `(decoded, incomplete_codepoint)` where the second element is true
/// when the buffer ends mid-codepoint and the caller should wait for the next
/// chunk before scanning.
fn decode_chunk_with_carry(carry: &mut Vec<u8>, chunk: &[u8]) -> Result<(String, bool), ()> {
    carry.extend_from_slice(chunk);
    match std::str::from_utf8(carry) {
        Ok(s) => {
            let s = s.to_string();
            carry.clear();
            Ok((s, false))
        }
        Err(e) => {
            if e.error_len().is_some() {
                // Genuinely malformed bytes — e.g. 0xFF can never start a
                // valid codepoint regardless of what follows. This is
                // corruption, not a split, so surface it as an error immediately.
                Err(())
            } else {
                // `error_len()` is None: the buffer ends mid-codepoint, which
                // is an ordinary chunk split. `valid_up_to()` gives the byte
                // index before the incomplete sequence; that prefix is solid.
                let valid_up_to = e.valid_up_to();
                let s = carry
                    .get(..valid_up_to)
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .unwrap_or_default()
                    .to_string();
                carry.drain(..valid_up_to);
                Ok((s, true))
            }
        }
    }
}

/// Formats an OpenAI-shaped SSE error event so that OpenAI-compatible clients
/// handle it via their existing error path.
fn sse_error_event(code: &str, message: &str) -> String {
    let body = serde_json::json!({
        "error": { "message": message, "type": code, "code": code }
    });
    format!("data: {body}\n\n")
}

/// Formats a `content_blocked` SSE error event carrying entity type labels
/// (never entity values).
fn sse_blocked_event(entities: &[String]) -> String {
    let message = format!(
        "response blocked: content matched a prohibited category ({})",
        entities.join(", ")
    );
    sse_error_event("content_blocked", &message)
}

/// Proxies one OpenAI-compatible request.
pub async fn handle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    path: axum::extract::OriginalUri,
    body: Bytes,
) -> Response {
    let fail_closed = state.engine.profile().fail_closed;
    state.metrics.requests_total.inc();

    // ── Request side ────────────────────────────────────────────────────────
    let mut json: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            // A body we cannot parse is a body we cannot inspect.
            return refuse(
                &state,
                fail_closed,
                Refusal::Undetermined(format!("request body is not JSON: {e}")),
            );
        }
    };

    let streaming = json.get("stream").and_then(Value::as_bool).unwrap_or(false);

    match scan_request(&state.engine, &mut json) {
        Ok(report) => {
            state.metrics.record(&report);
            if report.is_blocked() {
                state.metrics.blocked_total.inc();
                tracing::warn!(
                    entities = ?report.blocked,
                    "blocked request: prohibited content"
                );
                return Refusal::Blocked(report.blocked).into_response();
            }
            if report.scanned_fields == 0 {
                // Not an error, but it means the request went through
                // uninspected — the shape was one we do not know.
                state.metrics.uninspected_total.inc();
                tracing::warn!("request body had no recognised text fields; forwarded uninspected");
            }
        }
        Err(e) => {
            return refuse(
                &state,
                fail_closed,
                Refusal::Undetermined(format!("request scan failed: {e}")),
            );
        }
    }

    // ── Upstream ────────────────────────────────────────────────────────────
    let url = format!("{}{}", state.provider_base_url, path.0.path());
    let upstream = state
        .client
        .post(&url)
        .headers(forwarded_headers(&headers))
        .json(&json)
        .send()
        .await;

    let upstream = match upstream {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "upstream request failed");
            return Refusal::Undetermined(format!("upstream unavailable: {e}")).into_response();
        }
    };

    let status = upstream.status();
    let content_type = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();

    // Check if this is a streaming response BEFORE reading the full body.
    // For streaming SSE, use incremental scanning; for buffered responses, use
    // the traditional buffered path.
    let is_streaming = streaming || content_type.starts_with("text/event-stream");

    // ── Response side ───────────────────────────────────────────────────────

    // If streaming, handle incrementally without buffering the entire response.
    if is_streaming {
        // A non-2xx upstream response is an error object, not model output.
        // For streaming, we still need to reject non-2xx early since we can't
        // stream an error as SSE.
        if !status.is_success() {
            let upstream_body = match read_capped(upstream, state.max_body_bytes).await {
                Ok(b) => b,
                Err(e) => {
                    return refuse(
                        &state,
                        fail_closed,
                        Refusal::Undetermined(format!("could not read upstream response: {e}")),
                    );
                }
            };
            return (status, upstream_body).into_response();
        }

        // Stream the SSE response chunk-by-chunk using hold-back buffering.
        // fail_closed is consulted inside the spawned task; observe-only
        // logs the block and continues forwarding.  See
        // `scan_sse_streaming` for full semantics.
        return scan_sse_streaming(Arc::clone(&state), status, upstream, fail_closed);
    }

    // Non-streaming path: buffer the entire response before processing.
    let upstream_body = match read_capped(upstream, state.max_body_bytes).await {
        Ok(b) => b,
        Err(e) => {
            return refuse(
                &state,
                fail_closed,
                Refusal::Undetermined(format!("could not read upstream response: {e}")),
            );
        }
    };

    // A non-2xx upstream response is an error object, not model output. Pass it
    // through unmodified so the client sees the provider's own error.
    if !status.is_success() {
        return (status, upstream_body).into_response();
    }

    let mut response_json: Value = match serde_json::from_str(&upstream_body) {
        Ok(v) => v,
        Err(e) => {
            return refuse(
                &state,
                fail_closed,
                Refusal::Undetermined(format!("upstream response is not JSON: {e}")),
            );
        }
    };

    match scan_response(&state.engine, &mut response_json) {
        Ok(report) => {
            state.metrics.record(&report);
            if report.is_blocked() {
                state.metrics.blocked_total.inc();
                tracing::warn!(
                    entities = ?report.blocked,
                    "blocked response: prohibited content"
                );
                return Refusal::Blocked(report.blocked).into_response();
            }
        }
        Err(e) => {
            return refuse(
                &state,
                fail_closed,
                Refusal::Undetermined(format!("response scan failed: {e}")),
            );
        }
    }

    (status, axum::Json(response_json)).into_response()
}

/// Reads an upstream response body, refusing past `cap` bytes.
///
/// ⚠️ Used only for non-streaming (buffered) JSON responses and non-2xx error
/// responses. Streaming SSE responses use [`scan_sse_streaming`] instead.
///
/// This function holds the whole upstream body in memory because detection has
/// to see complete text for non-streaming bodies. `reqwest`'s `text()`/`bytes()`
/// apply **no limit**, and axum's `DefaultBodyLimit` bounds only the *inbound
/// request*, not what the upstream sends back. Without this cap a provider that
/// streams without stopping — malfunctioning, or hostile — grows the proxy's
/// heap until the pod is OOM-killed, taking down redaction for everyone.
///
/// Capping rather than streaming-through is the deliberate trade: buffered
/// detection is what stops an entity hiding in a token split, so the memory has
/// to be spent. What we control is the ceiling.
async fn read_capped(mut resp: reqwest::Response, cap: usize) -> Result<String, String> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if buf.len().saturating_add(chunk.len()) > cap {
                    return Err(format!("upstream response exceeded {cap} bytes"));
                }
                buf.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => return Err(e.to_string()),
        }
    }
    String::from_utf8(buf).map_err(|e| format!("upstream response is not UTF-8: {e}"))
}

/// Applies a refusal, honouring the profile's fail-closed setting.
///
/// On an `observe-only` profile an undetermined result is logged and the
/// request proceeds — that profile promises nothing. On every other profile it
/// is refused.
fn refuse(state: &Arc<AppState>, fail_closed: bool, refusal: Refusal) -> Response {
    if fail_closed {
        state.metrics.refused_total.inc();
        if let Refusal::Undetermined(ref why) = refusal {
            tracing::error!(reason = %why, "failing closed");
        }
        return refusal.into_response();
    }

    state.metrics.fail_open_total.inc();
    tracing::warn!("redaction indeterminate on a non-fail-closed profile; continuing");
    (StatusCode::BAD_GATEWAY, "redaction unavailable").into_response()
}

/// The subset of client headers forwarded upstream.
///
/// `authorization` is forwarded so the upstream gateway performs its own
/// authentication exactly as it would without this proxy in the path — this
/// service authenticates nobody and deliberately holds no credential of its
/// own. Hop-by-hop headers and anything that would confuse the upstream about
/// body framing are dropped.
fn forwarded_headers(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for name in [header::AUTHORIZATION, header::ACCEPT, header::USER_AGENT] {
        if let Some(v) = headers.get(&name) {
            out.insert(name, v.clone());
        }
    }
    // Propagate the caller's trace context so the upstream span joins the same
    // trace rather than starting a new one.
    for name in ["traceparent", "tracestate", "x-request-id"] {
        if let (Some(v), Ok(hn)) = (headers.get(name), header::HeaderName::try_from(name)) {
            out.insert(hn, v.clone());
        }
    }
    out
}

/// Records a report's counts. Kept here so both request and response paths
/// report identically.
impl Metrics {
    pub fn record(&self, report: &ScanReport) {
        if report.redactions > 0 {
            self.redactions_total.inc_by(report.redactions as u64);
        }
        self.scanned_fields_total
            .inc_by(report.scanned_fields as u64);
    }
}
