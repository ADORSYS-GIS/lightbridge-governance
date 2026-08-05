//! Incremental, SSE-frame-aware redaction over a streamed completion.
//!
//! [`SseHoldBack`] is the **incremental SSE scanning path** — the production
//! streaming target for the response direction. See the crate-level docs
//! ([`crate`]) for the full two-path architecture (buffered request, incremental
//! streaming response) and the trade-off that makes `SseHoldBack` the right
//! choice for real-time streaming at scale.
//!
//! # How it relates to [`crate::HoldBack`] and [`crate::scan_sse`]
//!
//! [`crate::HoldBack`] closes the *latency* gap for streaming (hold back a bounded
//! window instead of buffering the whole response) but not the *framing* gap: it
//! scans raw text with no notion of SSE structure, so a redaction operator's
//! replacement can in principle land inside the JSON surrounding `delta.content`
//! rather than only inside the content string itself, corrupting that frame.
//! [`crate::scan_sse`] (the buffered path, in the [`crate::streaming`] module) avoids
//! this by extracting exactly `delta.content` before touching anything —
//! [`SseHoldBack`] gets the same precision incrementally.
//!
//! # The approach
//!
//! Each choice index gets its own ephemeral redactor, which accumulates that
//! choice's `delta.content` text and — like [`crate::HoldBack`] — releases a
//! prefix once nothing still open can extend into it. The one addition:
//! **a release is always snapped down to a whole SSE frame boundary.** A
//! frame's content is never split across two release batches, so "attach the
//! released text to one frame, blank the others" is always exact — never a
//! partial string glued mid-frame.
//!
//! Frames that carry no redactable content (structural SSE lines, `[DONE]`,
//! role/`finish_reason`-only chunks) have nothing to wait on and are ready
//! immediately. But they still leave in **strict arrival order** — a ready
//! frame behind a still-held one waits its turn, so the reassembled stream's
//! frame sequence is never reordered.
//!
//! # Bounded memory
//!
//! The hold-back window (see [`DEFAULT_WINDOW`](crate::DEFAULT_WINDOW)) bounds
//! memory per concurrent stream. Ten concurrent 100 MB streams use roughly
//! ten × 4 KB (~40 KB total), not ten × 100 MB. The window must be large enough
//! to exceed the longest [`Action::Block`][crate::profile::Action] entity so no
//! credential is partially released before the block fires.


//! # What this still does not solve
//!
//! Multi-choice (`n > 1`) interleaving is handled correctly but is
//! unexercised against real multi-choice traffic — this platform's clients
//! (opencode, Kilo-Code, LibreChat) stream `n = 1`.

use std::collections::{HashMap, VecDeque};

use serde_json::Value;

use crate::{
    engine::{Engine, Span, Verdict},
    error::Result,
    holdback::safe_prefix,
    profile::Action,
    streaming::{ClassifiedLine, classify_line, delta_contents, set_delta_content},
};

/// What the caller should do after feeding a chunk. Mirrors
/// [`crate::Emit`]'s shape deliberately, so callers migrating from
/// [`crate::HoldBack`] to this type change only which methods they call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEmit {
    /// Nothing is safe to release yet. Keep reading.
    Nothing,
    /// Release this SSE text downstream verbatim (complete `data: …\n`
    /// lines and any passthrough lines, in original order).
    Release(String),
    /// A blocking entity appeared. Stop the stream and send an error; do not
    /// release this or any later text. Carries entity *types*, never values.
    Blocked(Vec<String>),
}

/// One queued line, held until its content (if any) is resolved.
enum Frame {
    Passthrough(String),
    Data(Value),
}

/// Incremental SSE redactor. Feed raw response bytes with [`Self::push`],
/// then [`Self::flush`] once at end-of-stream.
pub struct SseHoldBack {
    window: usize,
    line_buf: String,
    frame_seq: u64,
    /// Frame ids in arrival order. A frame leaves the front only once ready.
    order: VecDeque<u64>,
    frames: HashMap<u64, Frame>,
    /// Remaining choice-resolutions a `Data` frame is waiting on. Absent (or
    /// zero) means ready; only frames with redactable content ever gain an
    /// entry above zero.
    awaiting: HashMap<u64, usize>,
    choices: HashMap<u64, ChoiceRedactor>,
    blocked: bool,
}

impl SseHoldBack {
    /// Creates an SSE hold-back retaining `window` bytes of each choice's
    /// content before release. See [`crate::DEFAULT_WINDOW`] for the sizing
    /// rationale — the same one applies here.
    #[must_use]
    pub fn with_window(window: usize) -> Self {
        Self {
            window,
            line_buf: String::new(),
            frame_seq: 0,
            order: VecDeque::new(),
            frames: HashMap::new(),
            awaiting: HashMap::new(),
            choices: HashMap::new(),
            blocked: false,
        }
    }

    /// Total spans rewritten so far, across every choice.
    #[must_use]
    pub fn redactions(&self) -> usize {
        self.choices.values().map(|c| c.redactions).sum()
    }

    /// Feeds the next slice of raw response bytes (already UTF-8 decoded)
    /// and returns whatever SSE text is now safe to send.
    ///
    /// # Errors
    ///
    /// Propagates engine failures. On a `fail_closed` profile the caller must
    /// terminate the stream rather than forward unscanned text.
    pub fn push(&mut self, engine: &Engine, text: &str) -> Result<SseEmit> {
        if self.blocked {
            return Ok(SseEmit::Nothing);
        }
        self.line_buf.push_str(text);
        while let Some(nl) = self.line_buf.find('\n') {
            let line = self.line_buf[..=nl].to_string();
            self.line_buf.drain(..=nl);
            if let Some(entities) = self.ingest_line(engine, &line)? {
                self.block(entities.clone());
                return Ok(SseEmit::Blocked(entities));
            }
        }
        {
            let out = self.drain_ready();
            Ok(self.emit(out))
        }
    }

    /// Drains everything still held at end-of-stream.
    ///
    /// Any trailing bytes with no terminating `\n` (a truncated upstream
    /// response — real OpenAI streams always end each line with `\n\n`) are
    /// processed as one final line rather than silently dropped.
    ///
    /// # Errors
    ///
    /// Propagates engine failures, as [`Self::push`] does.
    pub fn flush(&mut self, engine: &Engine) -> Result<SseEmit> {
        if self.blocked {
            return Ok(SseEmit::Nothing);
        }
        if !self.line_buf.is_empty() {
            let line = std::mem::take(&mut self.line_buf);
            if let Some(entities) = self.ingest_line(engine, &line)? {
                self.block(entities.clone());
                return Ok(SseEmit::Blocked(entities));
            }
        }

        let indices: Vec<u64> = self.choices.keys().copied().collect();
        for index in indices {
            while let Some(choice) = self.choices.get_mut(&index) {
                let result = choice.flush(engine, self.window)?;
                match result {
                    ChoicePush::Nothing => break,
                    ChoicePush::Blocked(entities) => {
                        self.block(entities.clone());
                        return Ok(SseEmit::Blocked(entities));
                    }
                    ChoicePush::Release { text, frame_ids } => {
                        self.apply_release(index, &text, &frame_ids);
                    }
                }
            }
        }
        {
            let out = self.drain_ready();
            Ok(self.emit(out))
        }
    }

    fn emit(&self, out: String) -> SseEmit {
        if out.is_empty() {
            SseEmit::Nothing
        } else {
            SseEmit::Release(out)
        }
    }

    fn block(&mut self, _entities: Vec<String>) {
        self.blocked = true;
        self.order.clear();
        self.frames.clear();
        self.awaiting.clear();
        self.choices.clear();
        self.line_buf.clear();
    }

    /// Classifies and enqueues one complete line, pushing any content it
    /// carries into the owning choice's redactor.
    ///
    /// Returns the blocking entity list if this line's content must not
    /// leave — the caller stops the whole stream, not just this frame,
    /// since a block anywhere blocks everything not yet released.
    fn ingest_line(&mut self, engine: &Engine, line: &str) -> Result<Option<Vec<String>>> {
        let frame_id = self.frame_seq;
        self.frame_seq += 1;

        match classify_line(line) {
            ClassifiedLine::Passthrough => {
                self.frames
                    .insert(frame_id, Frame::Passthrough(line.to_string()));
                self.order.push_back(frame_id);
                Ok(None)
            }
            ClassifiedLine::Data(chunk) => {
                let contributions = delta_contents(&chunk);
                self.frames.insert(frame_id, Frame::Data(chunk));
                self.order.push_back(frame_id);
                if contributions.is_empty() {
                    // No content field on this frame — nothing to wait on.
                    return Ok(None);
                }
                self.awaiting.insert(frame_id, contributions.len());
                for (index, content) in contributions {
                    let choice = self.choices.entry(index).or_default();
                    match choice.push(engine, self.window, frame_id, &content)? {
                        ChoicePush::Nothing => {}
                        ChoicePush::Blocked(entities) => return Ok(Some(entities)),
                        ChoicePush::Release { text, frame_ids } => {
                            self.apply_release(index, &text, &frame_ids);
                        }
                    }
                }
                Ok(None)
            }
        }
    }

    /// Writes a choice's released text onto the first frame in the covered
    /// batch and blanks the rest — the same redistribution rule
    /// [`crate::streaming::rewrite_deltas`] uses buffered, applied to one
    /// resolved batch instead of the whole stream.
    fn apply_release(&mut self, index: u64, text: &str, frame_ids: &[u64]) {
        for (i, &fid) in frame_ids.iter().enumerate() {
            let replacement = if i == 0 { text } else { "" };
            if let Some(Frame::Data(chunk)) = self.frames.get_mut(&fid) {
                set_delta_content(chunk, index, replacement);
            }
            if let Some(count) = self.awaiting.get_mut(&fid) {
                *count = count.saturating_sub(1);
            }
        }
    }

    /// Pops and serialises every ready frame from the front of the queue, in
    /// order, stopping at the first frame still awaiting a resolution.
    fn drain_ready(&mut self) -> String {
        let mut out = String::new();
        while let Some(&fid) = self.order.front() {
            let ready = self.awaiting.get(&fid).copied().unwrap_or(0) == 0;
            if !ready {
                break;
            }
            self.order.pop_front();
            self.awaiting.remove(&fid);
            if let Some(frame) = self.frames.remove(&fid) {
                match frame {
                    Frame::Passthrough(raw) => out.push_str(&raw),
                    Frame::Data(chunk) => {
                        out.push_str("data: ");
                        out.push_str(&serde_json::to_string(&chunk).unwrap_or_default());
                        out.push('\n');
                    }
                }
            }
        }
        out
    }
}

/// What one choice's redactor decided after a push or flush.
enum ChoicePush {
    Nothing,
    /// `frame_ids` is the whole-frame-aligned batch now safe to release, in
    /// order. `text` is the redacted content for the batch as a whole — the
    /// caller attaches it to `frame_ids[0]` and blanks the rest.
    Release {
        text: String,
        frame_ids: Vec<u64>,
    },
    Blocked(Vec<String>),
}

/// Per-choice incremental redactor: like [`crate::HoldBack`], but every
/// release is snapped down to a whole SSE frame boundary rather than an
/// arbitrary byte offset.
#[derive(Default)]
struct ChoiceRedactor {
    pending: String,
    /// `(end offset within `pending`, frame id)`, ascending by offset.
    boundaries: VecDeque<(usize, u64)>,
    redactions: usize,
}

impl ChoiceRedactor {
    fn push(
        &mut self,
        engine: &Engine,
        window: usize,
        frame_id: u64,
        text: &str,
    ) -> Result<ChoicePush> {
        self.pending.push_str(text);
        self.boundaries.push_back((self.pending.len(), frame_id));
        self.advance(engine, window, false)
    }

    fn flush(&mut self, engine: &Engine, window: usize) -> Result<ChoicePush> {
        self.advance(engine, window, true)
    }

    fn advance(&mut self, engine: &Engine, window: usize, final_chunk: bool) -> Result<ChoicePush> {
        if self.pending.is_empty() {
            return Ok(ChoicePush::Nothing);
        }

        let spans: Vec<Span> = engine.detect(&self.pending)?;

        // Same rule as `HoldBack::advance`: a block anywhere in the pending
        // buffer stops everything, checked before any cut is chosen.
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
            self.boundaries.clear();
            return Ok(ChoicePush::Blocked(entities));
        }

        // At end-of-stream nothing more can arrive to extend an entity, so
        // the window no longer applies — everything held is releasable
        // (subject only to the span-safety walk-back, which cannot retreat
        // the cut below `pending.len()` since no span can extend past the
        // text it was detected in).
        let effective_window = if final_chunk { 0 } else { window };

        // Find the largest cut that is BOTH span-safe and frame-boundary
        // aligned. These two constraints interact: snapping a span-safe cut
        // DOWN to the nearest frame boundary can re-enter a span the
        // original cut had safely cleared (a boundary can sit strictly
        // inside a detection that straddles it, e.g. an email split so that
        // frame 0 ends mid-entity). So this is a fixed point, not a single
        // pass: recompute span-safety against each smaller candidate until
        // it stops moving. `candidate` only ever shrinks and boundaries are
        // finite, so this always terminates.
        let mut candidate = self.pending.len();
        let cut = loop {
            let safe = safe_prefix(&self.pending, &spans, effective_window, candidate);
            let mut boundary = 0usize;
            for &(offset, _) in &self.boundaries {
                if offset <= safe {
                    boundary = offset;
                } else {
                    break;
                }
            }
            if boundary == candidate || boundary == 0 {
                break boundary;
            }
            candidate = boundary;
        };
        if cut == 0 {
            return Ok(ChoicePush::Nothing);
        }

        let covered = self
            .boundaries
            .iter()
            .take_while(|&&(o, _)| o <= cut)
            .count();
        let frame_ids: Vec<u64> = self
            .boundaries
            .drain(..covered)
            .map(|(_, fid)| fid)
            .collect();
        for entry in &mut self.boundaries {
            entry.0 -= cut;
        }

        let head = self.pending[..cut].to_string();
        self.pending.drain(..cut);

        let out = match engine.scan(&head)? {
            Verdict::Clean => head,
            Verdict::Redacted { text, count } => {
                self.redactions += count;
                text
            }
            // Unreachable in practice: blocking was already checked over the
            // whole pending buffer above, and `head` is a prefix of it ending
            // exactly on a boundary the span walk-back already approved. Kept
            // as a real branch rather than `unreachable!()` — a fail-closed
            // component does not get to be optimistic about its own
            // invariants (see `HoldBack::advance`, same stance).
            Verdict::Blocked { entities } => {
                self.pending.clear();
                self.boundaries.clear();
                return Ok(ChoicePush::Blocked(entities));
            }
        };

        Ok(ChoicePush::Release {
            text: out,
            frame_ids,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{SseEmit, SseHoldBack};
    use crate::{engine::Engine, profile::Profile};

    fn engine() -> Engine {
        Engine::new(Profile::coding_assistant(), "salt").expect("engine")
    }

    fn frame(json: &str) -> String {
        format!("data: {json}\n\n")
    }

    /// Concatenates delta content the way any real client does — same
    /// helper shape as `streaming::tests::concat_deltas`, duplicated rather
    /// than shared across a `#[cfg(test)]` boundary between modules.
    fn concat_deltas(body: &str) -> String {
        let mut out = String::new();
        for line in body.lines() {
            let Some(p) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if p == "[DONE]" || p.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(p) else {
                continue;
            };
            if let Some(choices) = v.get("choices").and_then(|c| c.as_array()) {
                for choice in choices {
                    if let Some(s) = choice
                        .get("delta")
                        .and_then(|d| d.get("content"))
                        .and_then(|c| c.as_str())
                    {
                        out.push_str(s);
                    }
                }
            }
        }
        out
    }

    fn drive(hold: &mut SseHoldBack, engine: &Engine, chunks: &[&str]) -> String {
        let mut out = String::new();
        for c in chunks {
            match hold.push(engine, c).expect("push") {
                SseEmit::Release(s) => out.push_str(&s),
                SseEmit::Nothing => {}
                SseEmit::Blocked(e) => panic!("unexpected block: {e:?}"),
            }
        }
        match hold.flush(engine).expect("flush") {
            SseEmit::Release(s) => out.push_str(&s),
            SseEmit::Nothing => {}
            SseEmit::Blocked(e) => panic!("unexpected block: {e:?}"),
        }
        out
    }

    #[test]
    fn clean_stream_round_trips_its_text() {
        let e = engine();
        let mut h = SseHoldBack::with_window(64);
        let body = drive(
            &mut h,
            &e,
            &[
                &frame(r#"{"choices":[{"index":0,"delta":{"content":"let x"}}]}"#),
                &frame(r#"{"choices":[{"index":0,"delta":{"content":" = 1;"}}]}"#),
            ],
        );
        assert_eq!(concat_deltas(&body), "let x = 1;");
        assert_eq!(h.redactions(), 0);
    }

    /// The whole point of this module: with a window shorter than the
    /// entity, a naive byte-cut would split a frame's content across two
    /// release batches. The frame-boundary snap must prevent that,
    /// verified here by checking not just the concatenated text (which
    /// `HoldBack` already gets right) but that no PARTIAL frame content
    /// escapes before the whole entity is resolved.
    #[test]
    fn entity_split_across_frames_is_redacted_not_leaked() {
        let e = engine();
        let mut h = SseHoldBack::with_window(4); // shorter than "jane.doe@example.com"
        let body = drive(
            &mut h,
            &e,
            &[
                &frame(r#"{"choices":[{"index":0,"delta":{"content":"mail jane.doe@ex"}}]}"#),
                &frame(r#"{"choices":[{"index":0,"delta":{"content":"ample.com now"}}]}"#),
            ],
        );
        let text = concat_deltas(&body);
        assert!(
            !text.contains("jane.doe@example.com"),
            "email leaked: {text}"
        );
        assert!(!text.contains("jane.doe@ex"), "fragment leaked: {text}");
        assert!(text.contains("mail"), "surrounding text lost: {text}");
    }

    #[test]
    fn split_credential_blocks_the_stream() {
        let e = engine();
        let mut h = SseHoldBack::with_window(64);
        h.push(
            &e,
            &frame(r#"{"choices":[{"index":0,"delta":{"content":"ghp_abcdefghijkl"}}]}"#),
        )
        .expect("push");
        let out = h
            .push(
                &e,
                &frame(
                    r#"{"choices":[{"index":0,"delta":{"content":"mnopqrstuvwxyz0123456789"}}]}"#,
                ),
            )
            .expect("push");
        match out {
            SseEmit::Blocked(entities) => {
                assert!(
                    entities.iter().any(|s| s.contains("Secret")),
                    "{entities:?}"
                );
            }
            other => panic!("expected the split token to block, got {other:?}"),
        }
    }

    #[test]
    fn non_content_fields_survive() {
        let e = engine();
        let mut h = SseHoldBack::with_window(64);
        let body = drive(
            &mut h,
            &e,
            &[
                &frame(
                    r#"{"id":"c1","model":"glm","choices":[{"index":0,"delta":{"role":"assistant"}}]}"#,
                ),
                &frame(
                    r#"{"id":"c1","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}]}"#,
                ),
                &frame(
                    r#"{"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"total_tokens":5}}"#,
                ),
            ],
        );
        assert!(body.contains("\"role\":\"assistant\""), "{body}");
        assert!(body.contains("\"finish_reason\":\"stop\""), "{body}");
        assert!(body.contains("\"total_tokens\":5"), "{body}");
        assert!(body.contains("\"model\":\"glm\""), "{body}");
    }

    #[test]
    fn multiple_choices_do_not_bleed_into_each_other() {
        let e = engine();
        let mut h = SseHoldBack::with_window(64);
        let body = drive(
            &mut h,
            &e,
            &[&frame(
                r#"{"choices":[{"index":0,"delta":{"content":"all fine here"}},{"index":1,"delta":{"content":"write jane.doe@example.com"}}]}"#,
            )],
        );
        assert!(
            body.contains("all fine here"),
            "clean choice altered: {body}"
        );
        assert!(
            !body.contains("jane.doe@example.com"),
            "dirty choice not redacted: {body}"
        );
    }

    #[test]
    fn malformed_data_line_is_passed_through_not_dropped() {
        let e = engine();
        let mut h = SseHoldBack::with_window(64);
        let body = drive(&mut h, &e, &["data: not json at all\n\n"]);
        assert!(body.contains("not json at all"), "{body}");
    }

    #[test]
    fn frame_arrives_split_across_two_pushes() {
        // The line assembler must reassemble a frame whose bytes are split
        // mid-line by the transport, not just mid-frame at a `\n` boundary.
        let e = engine();
        let mut h = SseHoldBack::with_window(64);
        let body = drive(
            &mut h,
            &e,
            &[
                r#"data: {"choices":[{"index":0,"delta":{"content":"hi"}}"#,
                "]}\n\n",
            ],
        );
        assert_eq!(concat_deltas(&body), "hi");
    }

    #[test]
    fn released_prefix_never_splits_a_single_frames_content() {
        // Directly checks the property `HoldBack` cannot offer: every
        // released `data:` line's content is either the full redacted text
        // or empty -- never a byte-level fragment of what that specific
        // frame originally carried.
        let e = engine();
        let mut h = SseHoldBack::with_window(2);
        let body = drive(
            &mut h,
            &e,
            &[
                &frame(r#"{"choices":[{"index":0,"delta":{"content":"abcdefghij"}}]}"#),
                &frame(r#"{"choices":[{"index":0,"delta":{"content":"klmnopqrst"}}]}"#),
            ],
        );
        // A window of 2 is shorter than either frame's own 10-byte content,
        // so each frame is already past the window the moment the NEXT
        // frame arrives — they are free to release individually rather
        // than merge into one batch. The invariant under test is not "one
        // specific batch shape" but "never a byte-fragment of what a frame
        // originally carried": every non-empty value must be a WHOLE
        // frame's content, alone or concatenated with whichever neighbours
        // shared its release batch — never a prefix/suffix of one.
        let valid = ["", "abcdefghij", "klmnopqrst", "abcdefghijklmnopqrst"];
        for line in body.lines() {
            let Some(p) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            let v: serde_json::Value = serde_json::from_str(p).expect("json");
            let content = v["choices"][0]["delta"]["content"].as_str().unwrap_or("");
            assert!(
                valid.contains(&content),
                "frame content was a byte-fragment of a frame's original text: {content:?}"
            );
        }
        assert_eq!(concat_deltas(&body), "abcdefghijklmnopqrst");
    }
}
