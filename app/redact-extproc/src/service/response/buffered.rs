//! Buffered (non-SSE) response-body handling and gzip helpers.
//!
//! Split out of `service.rs` (issue #180) along the "buffered response" seam:
//! the whole-body-at-`end_of_stream` scan path for anything whose
//! `Content-Type` was not `text/event-stream`, plus the gzip
//! decompress/re-compress helpers and the size cap both the buffered path
//! and the gzip-SSE path share.

use envoy_types::pb::envoy::{service::ext_proc::v3::ProcessingResponse, r#type::v3::StatusCode};
use governance_redact::{Engine, scan_response};

use super::super::{
    Direction, body_response, body_response_with_original_length, immediate_response, record,
    refuse_or_block,
};
use crate::metrics::Metrics;

pub(crate) const MAX_BUFFERED_RESPONSE_BYTES: usize = 33_554_432;

/// Ceiling on a buffered (non-SSE) response body, mirroring
/// `redact-gateway`'s `read_capped` cap. `SseHoldBack` bounds its own memory
/// to the hold-back window regardless of stream length, but the buffered
/// path accumulates the whole body before it can be scanned — the same
/// trade `redact-gateway::read_capped`'s doc explains — so without a
/// ceiling a provider that never sets `Content-Type: text/event-stream` but
/// streams without stopping would grow this buffer until the pod is
/// OOM-killed.
///
/// Decompress a gzip-compressed buffer in place. No-op on non-gzip data.
/// Caps decompressed output at `MAX_BUFFERED_RESPONSE_BYTES` to prevent
/// OOM from pathologically compressed input.
pub(crate) fn decompress_gzip(buf: &mut Vec<u8>) -> Result<(), String> {
    use std::io::Read;

    use flate2::read::GzDecoder;
    if buf.len() < 2 || buf.first() != Some(&0x1f) || buf.get(1) != Some(&0x8b) {
        return Ok(());
    }
    let cap = (buf.len() * 4).min(MAX_BUFFERED_RESPONSE_BYTES);
    let decoder = GzDecoder::new(&buf[..]);
    let mut out = Vec::with_capacity(cap);
    // Read through a bounded reader so allocation cannot balloon past the cap:
    // `take(MAX+1)` stops the stream once we've read one byte more than the
    // limit, so a compression bomb bails while decompressing rather than
    // allocating gigabytes and then failing after the fact.
    decoder
        .take((MAX_BUFFERED_RESPONSE_BYTES as u64) + 1)
        .read_to_end(&mut out)
        .map_err(|e| format!("gzip decompression failed: {e}"))?;
    // If the decompressed output exceeds the cap (pathologically compressible
    // input), fail rather than return a partial/oversized body.
    if out.len() > MAX_BUFFERED_RESPONSE_BYTES {
        return Err(format!(
            "decompressed output exceeds {MAX_BUFFERED_RESPONSE_BYTES} bytes",
        ));
    }
    *buf = out;
    Ok(())
}

/// Re-compress as gzip only if `content_encoding` is `"gzip"`.
pub(crate) fn compress_as_gzip(data: &[u8], content_encoding: Option<&str>) -> Option<Vec<u8>> {
    if !content_encoding.is_some_and(|ce| ce.eq_ignore_ascii_case("gzip")) {
        return None;
    }
    use std::io::Write;

    use flate2::{Compression, write::GzEncoder};
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).ok()?;
    enc.finish().ok()
}

/// Accumulates a non-SSE response body — a plain JSON completion or
/// embeddings response, or anything whose `Content-Type` was not
/// `text/event-stream` — and scans it in one pass at `end_of_stream`, the
/// same way `redact-gateway`'s buffered response path (`scan_response`)
/// does. Nothing is released to the client before then: every non-final
/// chunk answers with an empty `body_mutation`, and the whole redacted body
/// is attached to the final one. `SseHoldBack`'s frame-by-frame release
/// cannot be reused here — it only ever looks inside `data:` lines, and a
/// plain JSON body has none, which is exactly the gap this function closes.
pub(crate) fn handle_buffered_response_chunk(
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

    // The upstream may return gzip-compressed JSON. Decompress before JSON
    // parsing so that serde_json sees uncompressed text, not binary gzip.
    let original_encoding = content_encoding;
    let compressed_len = buf.len();
    if let Err(e) = decompress_gzip(buf) {
        tracing::error!(
            content_type,
            content_encoding,
            body_len = buf.len(),
            error = %e,
            "buffered response body gzip decompression failed"
        );
        return refuse_or_block(
            Direction::Response,
            engine,
            metrics,
            &format!("response body decompression failed: {e}"),
        );
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
            let redacted = serde_json::to_vec(&json).unwrap_or_else(|_| buf.clone());
            let output = compress_as_gzip(&redacted, original_encoding).unwrap_or(redacted);
            body_response_with_original_length(Direction::Response, output, Some(compressed_len))
        }
        Err(e) => refuse_or_block(
            Direction::Response,
            engine,
            metrics,
            &format!("response scan failed: {e}"),
        ),
    }
}
