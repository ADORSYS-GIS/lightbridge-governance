//! PII detection and redaction for the AI request path.
//!
//! This crate owns the *policy* — which entities matter, what happens to them,
//! and what happens when the detector fails — and delegates only "where are the
//! entities in this string" to the [`pii`] crate.
//!
//! # Why we own this
//!
//! The platform previously deployed an off-the-shelf redaction *gateway*
//! (`censgate/redact`) as a front proxy. That put a third party's binary in the
//! fail-closed path of every AI request, and its published release could not
//! execute at all — a glibc mismatch between its build and runtime images meant
//! no container ever started. A binary in that position has to be one we build,
//! test and can fix.
//!
//! Depending on [`pii`] as a *library* is a different proposition: it is a
//! bounded, pure-function dependency (text in, spans out) with no network, no
//! process supervision and no release engineering of ours riding on it. If it
//! stalls we can vendor or replace it without touching the request path.
//!
//! # Two paths, two shapes
//!
//! Redaction must cover both the request (outbound from a client) and the
//! response (inbound from the model), and the shapes of those two paths are
//! fundamentally different. Conflating them in the design leads to wrong
//! performance or safety trade-offs for one of them.
//!
//! ## Request path — buffered
//!
//! The request body arrives *completely* before any byte is forwarded
//! upstream. This is the right shape for a request, and neither deployment
//! mode requires otherwise:
//!
//! - **`redact-extproc`**: Envoy's `processingMode.request.body: Buffered` mode
//!   hands the ext_proc service exactly one `RequestBody` message covering the
//!   whole payload. One message means whole body, which means we scan before
//!   forwarding.
//! - **`redact-gateway`**: The upstream HTTP round-trip cannot begin until the
//!   entire request is read anyway. Parsing and scanning before `.send()` is
//!   free — no added latency.
//!
//! Buffering the request gives one safety property that is not available
//! incrementally: **no entity can hide across a field boundary or a chunk
//! boundary**. A credential streamed as `ghp_` + `ABCDEFGHIJ` across two request
//! chunks, or a name broken across two `content` fields, is one string by the
//! time we inspect it. We see the complete value and act on it as one.
//!
//! The request is rewritten *in place*: [`scan_request`] mutates the JSONValue,
//! replacing or blocking each detected span. The caller forwards the mutated
//! body, never the original.
//!
//! ## Response path — incremental (SseHoldBack)
//!
//! The LLM streams a completion token by token. Buffering the *entire*
//! response before forwarding any byte would mean time-to-first-token equals
//! time-to-last-token — a 20-second completion starts returning at second 20,
//! not second 0. That is the wrong trade-off for a response that is ultimately
//! clean.
//!
//! The alternative is *incremental scanning*: feed each chunk into the scanner,
//! hold back text until the scanner can confirm the held region is clean,
//! release safe text as chunks arrive. That is [`SseHoldBack`].
//!
//! ### How SseHoldBack works
//!
//! [`SseHoldBack`] is an SSE-frame-aware incremental redaction state machine.
//! It is not a raw-byte hold-back ([`HoldBack`]) — it understands SSE structure
//! and will not release a frame that carries a partial redactable entity.
//!
//! ```text
//! LLM token stream (chunks)
//!   ├─ data: {"choices":[{"delta":{"content":"Hello"}}]}
//!   ├─ data: {"choices":[{"delta":{"content":" my name is John Smith."}}]}
//!   └─ data: {"choices":[{"delta":{"content":" ..."}}]}
//!
//! SseHoldBack processes each chunk in order:
//!   Step 1: chunk arrives → appended to line buffer
//!           line buffer: "data: {..content..."
//!           line parsed → delta.content extracted → scanner.push(delta.content)
//!           Result: "Hello" is clean
//!           holdback buffer: empty
//!           Release: [nothing — nothing held]
//!
//!   Step 2: chunk arrives
//!           scanner finds "John Smith" → PERSON detected
//!           holdback buffer count: > 2 KB window threshold
//!           scanner scans held buffer → no overlap with PERSON span
//!           Release: [safe content]
//!
//!   Step N: end_of_stream → flush
//!           any remaining held text scanned and released
//! ```
//!
//! ### Bounded memory
//!
//! The hold-back window is **bounded** ([`DEFAULT_WINDOW`], ~2 KB). The buffer
//! never grows beyond the window regardless of how long the stream runs. Ten
//! concurrent 100 MB streams use roughly ten × the window size (~20 KB total),
//! not ten × 100 MB.
//!
//! Why the window must not be unbounded: if the held region were allowed to grow
//! as large as the response, we are back to buffering the whole stream — same
//! latency problem, same OOM risk. The window IS the latency budget: an entity
//! longer than the window could straddle the window boundary, which is why the
//! window must be large enough to exceed any realistic entity (`DEFAULT_WINDOW`
//! is ~2 KB; the longest standard entity is a private key at ~2,200 bytes).
//! Anything that does not fully fit in the window is held until `flush`
//! (end-of-stream), where the full buffer is available.
//!
//! ### Frame-aware release
//!
//! A release is always **snapped to a whole SSE frame boundary**. A
//! redacted entity cannot land mid-frame in the rewritten JSON. This keeps the
//! downstream parser's job tractable — it always receives complete, well-formed
//! JSON per frame.
//!
//! Frames that carry no redactable content (structural lines like `event:`,
//! `id:`, `retry:`, `data: [DONE]`, blank lines) pass through immediately
//! without waiting.
//!
//! Frames are released in **strict arrival order**. A complete frame behind a
//! held one waits its turn — the client never sees tokens in the wrong order.
//!
//! ### Blocking mid-stream
//!
//! If an entity [`crate::Action::Block`] is detected in a response
//! chunk (e.g. a model-turned credential or a personal identifier), the stream
//! is terminated at that point. The client receives an error, not un-scanned
//! content. Nothing further is sent to the client.
//!
//! # Which module does which path
//!
//! | Path | Module | Public API | Deployment |
//! |------|---------|-----------|-----------|
//! | Request | `payload` | [`crate::scan_request`] | Both (`redact-gateway` + `redact-extproc`) |
//! | Response (non-streaming) | `streaming` | [`crate::scan_response`] | `redact-gateway` |
//! | Response (streaming SSE) | `sse` | [`crate::SseHoldBack`] | `redact-extproc` via Envoy |
//! | Response (buffered SSE, safe default) | `streaming` | [`crate::scan_sse`] | `redact-gateway` |
//!
//! # Fail-closed on error
//!
//! Every scanner returns a [`Result`]. On a `fail_closed` profile (every profile
//! except `observe-only`) the caller must reject the exchange on error rather
//! than forward unsanitised content. The error itself is not the content — it is
//! a signal that the scanner could not confirm the content is clean. That signal
//! means withhold, not pass through.
//!
//! # What this crate does not do
//!
//! - **It does not detect personal names.** See
//!   [`profile::Profile::detects_names`] — the `pii` crate's NER entities need
//!   a model that its `candle-ner` feature does not ship. Pattern recognizers
//!   cover Email, Phone, SSN, CreditCard, Iban and similar structured entities.
//! - **It does not own chunk I/O.** The caller's async runtime (Axum's in
//!   `redact-gateway`, Tonic's in `redact-extproc`) reads chunks from the
//!   upstream connection and feeds them into [`crate::SseHoldBack::push`]; this crate
//!   does not open sockets or manage async streams.
//! - **The raw [`crate::HoldBack`] (not [`crate::SseHoldBack`]) is not SSE-aware.**
//!   It holds raw bytes and uses a simple span overlap check without SSE frame
//!   semantics. It predates [`crate::SseHoldBack`] and exists only as a building
//!   block — callers should use [`crate::SseHoldBack`] for any SSE workload.
//!
//! # Example
//!
//! ```
//! use governance_redact::{Engine, Profile, Verdict};
//!
//! let engine = Engine::new(Profile::coding_assistant(), "deployment-salt")?;
//!
//! // Ordinary code passes through untouched.
//! assert_eq!(engine.scan("let x = 1;")?, Verdict::Clean);
//!
//! // A credential stops the request outright.
//! let verdict = engine.scan("ghp_abcdefghijklmnopqrstuvwxyz0123456789")?;
//! assert!(verdict.is_blocked());
//! # Ok::<(), governance_redact::Error>(())
//! ```
//!
//! # Public API
//!
//! - [`Engine::scan`][crate::Engine::scan] — point-in-time scan of an arbitrary string
//! - [`crate::scan_request`] — scan and rewrite an OpenAI request JSON body in place
//! - [`crate::scan_response`] — scan and rewrite a non-streaming OpenAI response JSON
//!   body in place
//! - [`crate::scan_sse`] — scan and rewrite a complete SSE stream (buffered)
//! - [`crate::SseHoldBack`] — incremental SSE redaction with a bounded hold-back window

pub mod engine;
pub mod error;
pub mod holdback;
pub mod payload;
pub mod profile;
pub mod secrets;
pub mod sse;
pub mod streaming;

pub use engine::{Engine, Span, Verdict};
pub use error::{Error, Result};
pub use holdback::{DEFAULT_WINDOW, Emit, HoldBack};
pub use payload::{ScanReport, scan_request, scan_response};
pub use profile::{Action, Profile};
pub use sse::{SseEmit, SseHoldBack};
pub use streaming::{StreamOutcome, scan_sse};
