//! Per-stream state types for the `ExternalProcessor` service.
//!
//! Split out of `service.rs` (issue #180) along the "state" seam: the
//! processor handle, the direction/phase enums, and the response state
//! threaded across every chunk of one HTTP exchange. The message loop that
//! drives these lives in [`super::dispatch`]; the per-phase handlers live in
//! [`super::request`] and [`super::response`].

use std::sync::Arc;

use envoy_types::pb::envoy::{config::core::v3::HeaderValue, service::ext_proc::v3::HttpHeaders};
use governance_redact::{Engine, SseHoldBack};

use crate::metrics::Metrics;

/// Implements Envoy's `ExternalProcessor` service against a shared
/// [`Engine`].
pub struct RedactProcessor {
    pub(crate) engine: Arc<Engine>,
    pub(crate) metrics: Arc<Metrics>,
    pub(crate) response_window: usize,
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
pub(crate) enum Direction {
    Request,
    Response,
}

/// Per-stream state. One `process` call is one HTTP request/response pair;
/// nothing here is shared across calls.
pub(crate) enum Phase {
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
pub(crate) enum ResponseBodyMode {
    /// `Content-Type: text/event-stream`. Handled incrementally via
    /// [`SseHoldBack`].
    Sse,
    /// Everything else. The accumulated raw bytes, scanned as one JSON body
    /// at `end_of_stream` — see [`super::response::buffered`].
    Buffered(Vec<u8>),
}

/// State threaded across every `ResponseBody` chunk of one HTTP exchange.
pub(crate) struct ResponseState {
    pub(crate) hold: Box<SseHoldBack>,
    /// Redactions reported as of the last chunk, so the cumulative counter
    /// `SseHoldBack::redactions` can be turned into a per-chunk delta for
    /// the Prometheus counter. Only advances in [`ResponseBodyMode::Sse`].
    pub(crate) last_redactions: usize,
    /// Trailing bytes from the previous chunk that did not form a complete
    /// UTF-8 codepoint on their own. See [`super::response::decode_chunk_with_carry`].
    /// Only used in [`ResponseBodyMode::Sse`].
    pub(crate) utf8_carry: Vec<u8>,
    pub(crate) mode: ResponseBodyMode,
    /// Header VALUES only (never a body), captured purely for diagnostics on
    /// a buffered-response JSON-parse failure — see that function's own
    /// comment on why a body snippet must never be logged (AGENTS.md: never
    /// log a request/response body) even though these two headers alone are
    /// usually enough to tell "this was compressed" from "this genuinely
    /// isn't JSON" apart.
    pub(crate) content_type: Option<String>,
    pub(crate) content_encoding: Option<String>,
    /// Accumulates gzip-compressed SSE chunks. When `Content-Encoding: gzip`
    /// and the mode is `Sse`, compressed chunks cannot be decoded incrementally
    /// — they are buffered here instead. At `end_of_stream` the buffer is
    /// decompressed and fed through the SSE holdback for scanning.
    pub(crate) gzip_buf: Option<Vec<u8>>,
}

impl ResponseState {
    pub(crate) fn new(window: usize) -> Self {
        Self {
            hold: Box::new(SseHoldBack::with_window(window)),
            last_redactions: 0,
            utf8_carry: Vec::new(),
            // Safe default until (or unless) the response headers say
            // otherwise — see the module doc's "Buffered" bullet.
            mode: ResponseBodyMode::Buffered(Vec::new()),
            content_type: None,
            content_encoding: None,
            gzip_buf: None,
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
    pub(crate) fn set_mode_from_headers(&mut self, headers: &HttpHeaders) {
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
        let mut content_type: Option<String> = None;
        let mut content_encoding: Option<String> = None;
        for h in &hm.headers {
            let val = hdr_val(h);
            if h.key.eq_ignore_ascii_case("content-type") {
                content_type = Some(val);
            } else if h.key.eq_ignore_ascii_case("content-encoding") {
                content_encoding = Some(val);
            }
        }
        let is_sse = content_type
            .as_deref()
            .is_some_and(|ct| ct.to_ascii_lowercase().starts_with("text/event-stream"));
        let is_gzip = content_encoding
            .as_deref()
            .is_some_and(|ce: &str| ce.eq_ignore_ascii_case("gzip"));
        if is_sse {
            self.mode = ResponseBodyMode::Sse;
        }
        // For gzip-compressed SSE, chunks arrive as binary ciphertext and
        // cannot be decoded as UTF-8 incrementally. Buffer them here and
        // decompress+scan at end_of_stream.
        if is_sse && is_gzip {
            self.gzip_buf = Some(Vec::new());
        }
        self.content_type = content_type;
        self.content_encoding = content_encoding;
    }
}
