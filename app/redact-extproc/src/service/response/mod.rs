//! Response-body handling for the `ExternalProcessor` service.
//!
//! Split out of `service.rs` (issue #180) along the "response" seam. The
//! response streams under `Streamed` mode; which of the two shapes a given
//! response actually has is resolved from the upstream `Content-Type` header
//! (see [`super::state::ResponseState::set_mode_from_headers`]) into either
//! the incremental SSE path ([`sse`]) or the whole-body buffered path
//! ([`buffered`]). This module owns the per-chunk dispatcher and the UTF-8
//! carry helper the SSE path uses.

use buffered::handle_buffered_response_chunk;
use envoy_types::pb::envoy::service::ext_proc::v3::ProcessingResponse;
use governance_redact::Engine;
use sse::{handle_gzip_sse_chunk, handle_sse_chunk};

use super::{ResponseBodyMode, ResponseState};
use crate::metrics::Metrics;

mod buffered;
mod sse;

// Consumed only by the test modules via the `service` re-export.
#[cfg(test)]
pub(crate) use buffered::{MAX_BUFFERED_RESPONSE_BYTES, decompress_gzip};

/// Dispatches one response chunk to whichever handling mode
/// [`ResponseState::set_mode_from_headers`] resolved for this exchange.
///
/// See the module doc for why a response is not assumed to be SSE just
/// because it arrived through `processingMode.response.body: Streamed`.
pub(crate) fn handle_response_chunk(
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

    // For gzip-compressed SSE, chunks arrive as binary ciphertext that cannot
    // be decoded as UTF-8 incrementally. Buffer them, decompress+scan at
    // end_of_stream via the SSE holdback.
    if state.gzip_buf.is_some() {
        return handle_gzip_sse_chunk(engine, metrics, state, chunk, end_of_stream);
    }

    handle_sse_chunk(engine, metrics, state, chunk, end_of_stream)
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
pub(crate) fn decode_chunk_with_carry(carry: &mut Vec<u8>, chunk: &[u8]) -> Result<String, ()> {
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
