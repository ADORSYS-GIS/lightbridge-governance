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
//! - **Buffered** (anything else, including a missing or unrecognised
//!   Content-Type): accumulated in full and scanned in one pass at
//!   `end_of_stream`, mirroring `redact-gateway`'s non-streaming response
//!   path. This is the fail-closed default — `SseHoldBack` only ever
//!   examines `data:` lines, so feeding it a plain JSON body (because SSE
//!   was wrongly assumed) would release every byte as
//!   `Frame::Passthrough` with zero calls to `engine.scan`. That was a
//!   real gap: prior to this mode existing, every non-SSE response body
//!   ext_proc's `Streamed` setting handed us went out completely
//!   unscanned. An ambiguous Content-Type buffers rather than streams —
//!   "unknown" routes to the branch that inspects the whole body before
//!   releasing anything, not to the one that assumes it is safe to
//!   stream through.
//!
//! **Accept-Encoding is left intact.** The extproc does not strip or modify
//! the upstream-bound `Accept-Encoding` header. The upstream decides how to
//! encode its response based on the header it receives:
//!
//! - `stream: false` (non-streaming JSON): the upstream typically returns
//!   gzip-compressed JSON, which the buffered path decompresses via
//!   [`decompress_gzip`] before JSON parsing.
//! - `stream: true` (SSE): the upstream returns plaintext events regardless
//!   of `Accept-Encoding` — gzip is never applied to streaming responses for
//!   this provider. The incremental `SseHoldBack` path handles it directly.
//!
//! In either case the response-path code in [`handle_response_chunk`]
//! resolves the correct handling from the response headers (Content-Type
//! and Content-Encoding), not from what the request carried.
//!
//! **Streaming "aggregation" is an Envoy delivery property, not an extproc
//! bug.** Envoy's `processingMode.response.body: Streamed` sends upstream
//! DATA frames to the extproc as gRPC streaming messages. For short
//! completions the entire SSE body often arrives in a single gRPC message
//! (`end_of_stream: true` on the first chunk), so the `SseHoldBack`
//! processes all frames at once and releases them together. The client sees
//! the full response at once not because the extproc aggregated anything,
//! but because Envoy delivered it all at once. Longer responses arrive in
//! multiple chunks and the holdback releases them incrementally. The
//! extproc cannot pace output faster than Envoy delivers input.
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
//!
//! ## Layout
//!
//! Split out of a single 1588-line `service.rs` (issue #180) along its real
//! seams:
//!
//! - [`state`] — the processor handle and per-stream state types.
//! - [`dispatch`] — the gRPC message loop and per-message state machine.
//! - [`request`] — request-body handling.
//! - [`response`] — response-body handling (SSE and buffered).
//! - [`builders`] — the shared reply/refusal builders.

mod builders;
mod dispatch;
mod request;
mod response;
mod state;

pub(crate) use builders::{
    body_response, body_response_with_original_length, immediate_response, record, refuse_or_block,
};
pub(crate) use dispatch::continue_headers;
// The symbols below are consumed only by the test modules (which
// `use super::*`), so they are gated to the test build to avoid
// `unused_imports` in the shipping binary.
#[cfg(test)]
pub(crate) use dispatch::dispatch;
#[cfg(test)]
pub(crate) use envoy_types::pb::envoy::{
    config::core::v3::HeaderValue,
    service::ext_proc::v3::{
        BodyMutation, BodyResponse, CommonResponse, HeaderMutation, HeadersResponse, HttpHeaders,
        ProcessingResponse, body_mutation, common_response::ResponseStatus,
        processing_request::Request as Req, processing_response::Response as Resp,
    },
    r#type::v3::StatusCode,
};
#[cfg(test)]
pub(crate) use governance_redact::Engine;
pub(crate) use request::handle_request_body;
pub(crate) use response::handle_response_chunk;
#[cfg(test)]
pub(crate) use response::{MAX_BUFFERED_RESPONSE_BYTES, decode_chunk_with_carry, decompress_gzip};
pub use state::RedactProcessor;
pub(crate) use state::{Direction, Phase, ResponseBodyMode, ResponseState};

#[cfg(test)]
pub(crate) use crate::metrics::Metrics;

#[cfg(test)]
mod tests;
