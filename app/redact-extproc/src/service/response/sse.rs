//! Incremental SSE response-body handling.
//!
//! Split out of `service.rs` (issue #180) along the "SSE streaming" seam.
use envoy_types::pb::envoy::{service::ext_proc::v3::ProcessingResponse, r#type::v3::StatusCode};
use governance_redact::{Engine, Error, SseEmit, SseHoldBack};

use super::{
    super::{Direction, ResponseState, body_response, immediate_response, refuse_or_block},
    buffered::{MAX_BUFFERED_RESPONSE_BYTES, compress_as_gzip, decompress_gzip},
    decode_chunk_with_carry,
};
use crate::metrics::Metrics;

/// Pushes `text` through the holdback and, at `end_of_stream`, flushes the
/// remaining window, combining the two emissions into one.
fn push_and_flush(
    hold: &mut Box<SseHoldBack>,
    engine: &Engine,
    text: &str,
) -> Result<SseEmit, Error> {
    hold.push(engine, text).and_then(|first| {
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
            // block before computing any cut, in both `push` and `flush`.
            // Since `first` (this push) was not `Blocked`, no blocking span
            // exists anywhere in `pending` at that point — and `safe_cut`
            // never lets a span straddle the cut, so the text `flush` sees
            // afterward is a strict, span-clean subset of what `push`
            // already scanned clean. A block surfacing only here would mean
            // the same text was clean moments ago and is not now, with
            // nothing appended between the two calls.
            (SseEmit::Release(_), SseEmit::Blocked(_)) => {
                unreachable!(
                    "flush cannot find a block that push's scan of the same pending buffer did not"
                )
            }
        })
    })
}

/// Turns the cumulative `SseHoldBack::redactions` counter into a per-chunk
/// delta for the Prometheus counter.
fn record_delta(metrics: &Metrics, hold: &mut Box<SseHoldBack>, last_redactions: &mut usize) {
    let delta = hold.redactions().saturating_sub(*last_redactions);
    if delta > 0 {
        metrics.redactions_total.inc_by(delta as u64);
        *last_redactions = hold.redactions();
    }
}

/// Turns an `SseEmit` into the Envoy reply: release the bytes (through
/// `release`, which lets the gzip path re-compress), block, or fail closed.
fn emit_response(
    engine: &Engine,
    metrics: &Metrics,
    emit: Result<SseEmit, Error>,
    release: impl FnOnce(String) -> Vec<u8>,
) -> ProcessingResponse {
    match emit {
        Ok(SseEmit::Nothing) => body_response(Direction::Response, Vec::new()),
        Ok(SseEmit::Release(out)) => body_response(Direction::Response, release(out)),
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

/// Handles one chunk of a gzip-compressed SSE response. Chunks arrive as
/// binary ciphertext that cannot be decoded as UTF-8 incrementally, so they
/// are buffered and decompressed+scanned at `end_of_stream`, then
/// re-compressed to match `Content-Encoding: gzip`.
pub(crate) fn handle_gzip_sse_chunk(
    engine: &Engine,
    metrics: &Metrics,
    state: &mut ResponseState,
    chunk: &[u8],
    end_of_stream: bool,
) -> ProcessingResponse {
    let Some(buf) = state.gzip_buf.as_mut() else {
        // Only reachable if the dispatcher's gzip_buf check and this call
        // disagree; fail closed rather than release anything unscanned.
        return refuse_or_block(
            Direction::Response,
            engine,
            metrics,
            "gzip-SSE buffer not armed",
        );
    };
    if buf.len().saturating_add(chunk.len()) > MAX_BUFFERED_RESPONSE_BYTES {
        return refuse_or_block(
            Direction::Response,
            engine,
            metrics,
            &format!("gzip-SSE buffer exceeded {MAX_BUFFERED_RESPONSE_BYTES} bytes"),
        );
    }
    buf.extend_from_slice(chunk);
    if !end_of_stream {
        return body_response(Direction::Response, Vec::new());
    }
    if let Err(e) = decompress_gzip(buf) {
        tracing::error!(error = %e, "SSE gzip decompression failed");
        return refuse_or_block(
            Direction::Response,
            engine,
            metrics,
            &format!("SSE gzip decompression failed: {e}"),
        );
    }
    let Ok(text) = std::str::from_utf8(buf) else {
        return refuse_or_block(
            Direction::Response,
            engine,
            metrics,
            "decompressed SSE bytes are not valid UTF-8",
        );
    };
    let content_encoding = state.content_encoding.as_deref();
    let emit = push_and_flush(&mut state.hold, engine, text);
    record_delta(metrics, &mut state.hold, &mut state.last_redactions);
    emit_response(engine, metrics, emit, |out| {
        // We decompressed the upstream gzip stream to scan it, but the
        // exchange still carries `Content-Encoding: gzip`. Re-compress the
        // scanned (possibly redacted) SSE text so the delivered body matches
        // the header — a gzip-decoding client must not receive identity-
        // encoded bytes stamped as gzip.
        let bytes = out.into_bytes();
        compress_as_gzip(&bytes, content_encoding).unwrap_or(bytes)
    })
}

/// Handles one chunk of a plain (non-gzip) SSE response, feeding it through
/// the holdback incrementally.
pub(crate) fn handle_sse_chunk(
    engine: &Engine,
    metrics: &Metrics,
    state: &mut ResponseState,
    chunk: &[u8],
    end_of_stream: bool,
) -> ProcessingResponse {
    let Ok(text) = decode_chunk_with_carry(&mut state.utf8_carry, chunk) else {
        return refuse_or_block(
            Direction::Response,
            engine,
            metrics,
            "response bytes are not valid UTF-8",
        );
    };

    // A non-empty carry at end-of-stream means the response ended mid
    // codepoint with no further bytes ever coming to complete it — genuine
    // truncation, not a chunk-boundary artifact; fail closed rather than
    // silently drop the incomplete tail.
    if end_of_stream && !state.utf8_carry.is_empty() {
        return refuse_or_block(
            Direction::Response,
            engine,
            metrics,
            "response ended mid UTF-8 codepoint",
        );
    }

    let emit = if end_of_stream {
        push_and_flush(&mut state.hold, engine, &text)
    } else {
        state.hold.push(engine, &text)
    };
    record_delta(metrics, &mut state.hold, &mut state.last_redactions);

    emit_response(engine, metrics, emit, String::into_bytes)
}
