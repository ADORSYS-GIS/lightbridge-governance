//! Incremental redaction over a growing text stream.
//!
//! The buffered path ([`crate::scan_sse`]) scans a whole completion at once,
//! which catches everything but makes time-to-first-token equal to
//! time-to-*last*-token. That is the wrong trade for interactive traffic, and
//! it is not the trade an `ext_proc` filter is shaped for: Envoy hands us the
//! response body in chunks against a per-message timeout.
//!
//! This is the incremental alternative. It keeps a **hold-back window**: the
//! last `window` bytes of text stay unreleased, so an entity still growing at
//! the tail cannot be emitted half-written. Everything before that is scanned
//! and released as soon as it is safe.
//!
//! The cost is bounded and predictable: output lags input by at most `window`
//! bytes, never by the length of the completion.
//!
//! ## The rule that makes it safe
//!
//! An entity only ever grows to the **right** as more text arrives, so text is
//! safe to release once no detection can still extend into it. Concretely, we
//! never cut through a span: if a detection starts before the release point and
//! ends after it, the release point moves back to that detection's start and
//! the whole entity is held until it is complete.
//!
//! This is why [`Engine::detect`] exists. Given only rewritten text we could
//! not see that a credential began three bytes before the cut, and would emit
//! its first half — the exact leak this type is built to prevent.
//!
//! ## Choosing `window`
//!
//! It must exceed the longest entity that must be caught, or that entity can
//! straddle the boundary indefinitely. Our longest patterns are the credential
//! ones (a GitHub token allows 255 characters after its prefix); [`DEFAULT_WINDOW`]
//! is sized above them with room to spare. Too small silently weakens
//! detection; too large just adds latency. Prefer too large.

use crate::{
    engine::{Engine, Span},
    error::Result,
    profile::Action,
};

/// Default hold-back size in bytes.
///
/// Sized to exceed our longest [`Action::Block`] entity so no such entity can
/// straddle the window boundary and have its safe prefix released to the
/// client before detection fires. A 4 KB window accommodates a standard PKCS#8
/// RSA 4096 PEM (~3,300 bytes) with margin; the next practical size is
/// 8,192 bytes. A larger window means more memory held per concurrent stream
/// and higher worst-case output lag (stream hangs for the fill time = window /
/// token-arrival-rate), so prefer not to raise this further without need.
pub const DEFAULT_WINDOW: usize = 4096;

/// What the caller should do after feeding a chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Emit {
    /// Nothing is safe to release yet. Keep reading.
    Nothing,
    /// Release this text downstream. Already redacted.
    Release(String),
    /// A blocking entity appeared. Stop the stream and send an error; do not
    /// release this or any later text. Carries entity *types*, never values.
    Blocked(Vec<String>),
}

/// Incremental redactor over a growing stream of assistant text.
///
/// Feed it decoded content with [`HoldBack::push`], then call
/// [`HoldBack::flush`] once at end-of-stream to drain the window.
#[derive(Debug)]
pub struct HoldBack {
    window: usize,
    pending: String,
    released: usize,
    redactions: usize,
}

impl HoldBack {
    /// Creates a hold-back buffer with [`DEFAULT_WINDOW`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_window(DEFAULT_WINDOW)
    }

    /// Creates a hold-back buffer retaining `window` bytes.
    ///
    /// A `window` of 0 is accepted but means every byte is released as soon as
    /// it is scanned, which cannot catch an entity split across chunks. It
    /// exists for tests, not for traffic.
    #[must_use]
    pub const fn with_window(window: usize) -> Self {
        Self {
            window,
            pending: String::new(),
            released: 0,
            redactions: 0,
        }
    }

    /// Total bytes released downstream so far.
    #[must_use]
    pub const fn released_bytes(&self) -> usize {
        self.released
    }

    /// Total spans rewritten so far.
    #[must_use]
    pub const fn redactions(&self) -> usize {
        self.redactions
    }

    /// Feeds the next piece of text and returns whatever is now safe to send.
    ///
    /// # Errors
    ///
    /// Propagates engine failures. On a `fail_closed` profile the caller must
    /// terminate the stream rather than forward unscanned text.
    pub fn push(&mut self, engine: &Engine, text: &str) -> Result<Emit> {
        self.pending.push_str(text);
        self.advance(engine, false)
    }

    /// Drains the window at end-of-stream.
    ///
    /// Everything still held is scanned and released, because no further text
    /// can arrive to extend an entity.
    ///
    /// # Errors
    ///
    /// Propagates engine failures, as [`HoldBack::push`] does.
    pub fn flush(&mut self, engine: &Engine) -> Result<Emit> {
        self.advance(engine, true)
    }

    fn advance(&mut self, engine: &Engine, final_chunk: bool) -> Result<Emit> {
        if self.pending.is_empty() {
            return Ok(Emit::Nothing);
        }

        let spans = engine.detect(&self.pending)?;

        // Blocking beats everything, and beats it early: once we know the
        // stream carries something that must not leave, no further text is
        // released regardless of where it sits relative to the window.
        let blocking: Vec<String> = spans
            .iter()
            .filter(|s| s.action == Action::Block)
            .map(|s| s.entity.clone())
            .collect();
        if !blocking.is_empty() {
            let mut entities = blocking;
            entities.sort_unstable();
            entities.dedup();
            self.pending.clear();
            return Ok(Emit::Blocked(entities));
        }

        let cut = if final_chunk {
            self.pending.len()
        } else {
            self.safe_cut(&spans)
        };
        if cut == 0 {
            return Ok(Emit::Nothing);
        }

        // Safe because `cut` never falls inside a span and is floored to a
        // char boundary, so re-scanning the prefix alone sees whole entities.
        let head = self.pending[..cut].to_string();
        self.pending.drain(..cut);

        let out = match engine.scan(&head)? {
            crate::Verdict::Clean => head,
            crate::Verdict::Redacted { text, count } => {
                self.redactions += count;
                text
            }
            // Unreachable: blocking spans were handled above. Treated as a
            // block rather than assumed impossible — a fail-closed component
            // does not get to be optimistic about its own invariants.
            crate::Verdict::Blocked { entities } => {
                self.pending.clear();
                return Ok(Emit::Blocked(entities));
            }
        };

        self.released += out.len();
        Ok(Emit::Release(out))
    }

    /// The furthest byte offset that is safe to release.
    ///
    /// Starts at "everything except the window", then walks back so the cut
    /// never lands inside a detection that is still open at the boundary.
    fn safe_cut(&self, spans: &[Span]) -> usize {
        safe_prefix(&self.pending, spans, self.window, self.pending.len())
    }
}

/// The shared cut-selection rule: how far into `pending` it is safe to
/// release, given a hold-back `window` and an optional `ceiling` no cut may
/// exceed.
///
/// Factored out of [`HoldBack`] so [`crate::sse`] can reuse the exact same
/// span-safety walk-back while additionally constraining the cut to a whole
/// SSE frame boundary via `ceiling` — duplicating this algorithm would risk
/// the two hold-back strategies silently drifting apart on the one rule that
/// makes either of them safe.
///
/// `ceiling` lets a caller cap the cut below the window-implied point (an SSE
/// frame boundary, say) without duplicating the span walk-back. Passing
/// `pending.len()` imposes no extra constraint, matching plain byte-oriented
/// hold-back.
pub(crate) fn safe_prefix(pending: &str, spans: &[Span], window: usize, ceiling: usize) -> usize {
    let mut cut = pending.len().saturating_sub(window).min(ceiling);
    if cut == 0 {
        return 0;
    }
    for s in spans {
        if s.start < cut && s.end > cut {
            cut = s.start;
        }
    }
    // `Span` offsets are boundaries already; the window/ceiling arithmetic
    // above may not have landed on one.
    while cut > 0 && !pending.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

impl Default for HoldBack {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{Emit, HoldBack};
    use crate::{engine::Engine, profile::Profile};

    fn engine() -> Engine {
        Engine::new(Profile::coding_assistant(), "test-salt").expect("engine")
    }

    fn released(outs: &[Emit]) -> String {
        outs.iter()
            .filter_map(|e| match e {
                Emit::Release(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn clean_text_streams_through_unchanged() {
        let e = engine();
        let mut h = HoldBack::with_window(8);
        let mut outs = vec![];
        for chunk in ["fn main() ", "{ println!", "(\"hi\"); }"] {
            outs.push(h.push(&e, chunk).expect("push"));
        }
        outs.push(h.flush(&e).expect("flush"));
        assert_eq!(released(&outs), "fn main() { println!(\"hi\"); }");
    }

    /// The whole point: an email arriving across three chunks must still be
    /// redacted, and no fragment of it may be released early.
    #[test]
    fn entity_split_across_chunks_is_redacted_not_leaked() {
        let e = engine();
        let mut h = HoldBack::with_window(64);
        let mut outs = vec![];
        for chunk in ["contact jane", ".doe@examp", "le.com now"] {
            outs.push(h.push(&e, chunk).expect("push"));
        }
        outs.push(h.flush(&e).expect("flush"));

        let all = released(&outs);
        assert!(!all.contains("jane.doe@example.com"), "email leaked: {all}");
        assert!(!all.contains("jane.doe"), "fragment leaked: {all}");
        assert!(all.contains("contact"), "surrounding text lost: {all}");
    }

    /// A credential split across chunks must block, not merely redact.
    #[test]
    fn split_credential_blocks_the_stream() {
        let e = engine();
        let mut h = HoldBack::with_window(64);
        h.push(&e, "use token ghp_abcdefghij").expect("push");
        let last = h.push(&e, "klmnopqrstuvwxyz0123456789 now").expect("push");
        match last {
            Emit::Blocked(entities) => assert!(
                entities.iter().any(|s| s.contains("Secret")),
                "expected a Secret block, got {entities:?}"
            ),
            other => panic!("expected the split token to block, got {other:?}"),
        }
    }

    /// The test that actually exercises the walk-back in [`HoldBack::safe_cut`].
    ///
    /// ⚠️ The chunk-split tests above do NOT: their window exceeds the whole
    /// string, so the release point is 0 and nothing is ever cut. Both still
    /// passed with the walk-back deleted, which is worth remembering — six
    /// green tests said the guard worked and none of them touched it.
    ///
    /// Here the window (10) is deliberately *shorter* than the email (20), so
    /// the release point lands mid-entity and the walk-back is the only thing
    /// preventing `jane.doe@e` from being emitted.
    #[test]
    fn does_not_release_through_an_entity_straddling_the_boundary() {
        let e = engine();
        let mut h = HoldBack::with_window(10);

        // len 35, window 10 => naive cut at 25, inside the email at [15, 35).
        let out = h
            .push(&e, "please contact jane.doe@example.com")
            .expect("push");

        if let Emit::Release(ref s) = out {
            assert!(
                !s.contains("jane"),
                "released through the entity — the walk-back is not working: {s}"
            );
            assert_eq!(s, "please contact ", "should stop at the entity start");
        }

        let tail = h.flush(&e).expect("flush");
        let all = released(&[out, tail]);
        assert!(!all.contains("jane.doe@example.com"), "email leaked: {all}");
        assert!(!all.contains("jane.doe@e"), "fragment leaked: {all}");
    }

    /// Latency guarantee: output must not wait for end-of-stream. With a small
    /// window, clean text well past it is released while the stream continues.
    #[test]
    fn releases_before_end_of_stream() {
        let e = engine();
        let mut h = HoldBack::with_window(4);
        let first = h
            .push(&e, "the quick brown fox jumps over the lazy dog")
            .expect("push");
        assert!(
            matches!(first, Emit::Release(_)),
            "nothing released before flush — this is buffering, got {first:?}"
        );
    }

    /// Nothing may escape while the buffer is still inside the window.
    #[test]
    fn holds_everything_until_the_window_is_exceeded() {
        let e = engine();
        let mut h = HoldBack::with_window(1024);
        assert_eq!(h.push(&e, "short").expect("push"), Emit::Nothing);
        assert_eq!(h.released_bytes(), 0);
    }

    /// Multi-byte characters must never be cut mid-codepoint.
    #[test]
    fn does_not_split_a_utf8_codepoint() {
        let e = engine();
        let mut h = HoldBack::with_window(5);
        let mut outs = vec![];
        for chunk in ["héllo wörld ", "ünïcode ", "tëxt"] {
            outs.push(h.push(&e, chunk).expect("push"));
        }
        outs.push(h.flush(&e).expect("flush"));
        assert_eq!(released(&outs), "héllo wörld ünïcode tëxt");
    }
}
