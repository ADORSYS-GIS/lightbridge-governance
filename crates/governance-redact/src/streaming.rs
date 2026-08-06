//! Redacting a Server-Sent Events completion stream.
//!
//! This module is the **buffered SSE scanning path**. It scans an *already
//! complete* SSE stream — the whole upstream response is buffered in memory
//! before any byte is forwarded. See the crate-level docs ([`crate`]) for the
//! full two-path architecture (buffered request, incremental streaming response)
//! and the safety vs. latency trade-off that decides which path is used where.
//!
//! # Why buffered SSE
//!
//! The public entry point here ([`scan_sse`]) is the conservative default for the
//! response path. It is the safe counterpart to the buffered request scan
//! ([`crate::scan_request`]): because the entire stream is present before any byte is
//! forwarded, no entity can hide in a token split. A credential streamed as
//! `ghp_` + `ABC…` across chunks is one string by the time [`scan_sse`] looks
//! at it.
//!
//! The cost: time-to-first-token = time-to-last-token. A 20-second completion
//! begins returning at second 20, not second 0. The incremental path
//! ([`crate::sse::SseHoldBack`]) trades that latency back for bounded memory
//! by scanning chunk-by-chunk with a hold-back window. [`scan_sse`] is the safe
//! default; [`crate::sse::SseHoldBack`] is the production streaming target.
//!
//! # What is preserved
//!
//! Chunk **sequence** and every field on every chunk. The only values rewritten
//! are `choices[i].delta.content` and `choices[i].delta.tool_calls[j].function.arguments`
//! — tool-call arguments are model-authored JSON strings that routinely echo
//! user input straight back (see [`crate::payload::scan_message`]'s buffered
//! rule, which this mirrors), so they are as much a leak surface as content
//! and get the identical extraction and write-back treatment, per choice
//! *and* per tool-call index. Role announcements, `finish_reason`, `usage`,
//! `system_fingerprint` and unknown provider extensions pass through
//! untouched, because a client parsing them must still find what it expects.
//!
//! Because the text is coalesced for detection and then written back, the
//! redacted string lands on the **first** chunk that carried text for a given
//! choice/field and later fragments become empty strings. Clients therefore
//! receive fewer non-empty deltas than the provider sent — inherent to
//! buffering, and invisible to any client that concatenates deltas (which is
//! all of them).

use std::collections::HashMap;

use serde_json::Value;

use crate::{engine::Engine, error::Result, payload::ScanReport};

/// The sentinel terminating an OpenAI SSE stream.
pub(crate) const DONE: &str = "[DONE]";

/// Which streamed field within one choice a piece of extracted text came
/// from — `delta.content`, or one indexed tool call's
/// `function.arguments`. Tool calls stream fragmented across chunks exactly
/// like `content` does, so each tool-call index needs its own accumulator,
/// the same way each choice index does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ContentField {
    /// `choices[i].delta.content`
    Content,
    /// `choices[i].delta.tool_calls[j].function.arguments`, keyed by the
    /// tool call's own `index` field (not `j`, the array position — a
    /// provider is free to omit earlier indices on a later chunk).
    ToolCallArguments(u64),
}

/// A choice index plus which field within it. The unit of accumulation for
/// both the buffered path ([`scan_sse`]) and the incremental one
/// ([`crate::sse::SseHoldBack`]) — content and every tool call's arguments
/// are independent streams that must not be coalesced together.
pub(crate) type ContentKey = (u64, ContentField);

/// One parsed SSE line: either a JSON data payload we may rewrite, or a line
/// we pass through verbatim.
#[derive(Debug, Clone)]
enum Line {
    /// `data: {json}` — index into the parsed-chunk table.
    Data(usize),
    /// Anything else: `event:`, `id:`, `retry:`, comments, blank lines, and
    /// `data: [DONE]`. Preserved byte-for-byte.
    Passthrough(String),
}

/// One line classified the way [`scan_sse`] and [`crate::sse::SseHoldBack`]
/// both need to: a parsed `data:` JSON payload, raw text to pass through
/// unexamined, or a `data:` payload that failed to parse as JSON.
///
/// Shared so the two hold-back strategies agree on what counts as
/// redactable content — a divergence here would mean the buffered and
/// incremental paths redact different things from the same stream.
pub(crate) enum ClassifiedLine {
    Data(Value),
    Passthrough,
    /// A `data:` line whose payload is not valid JSON. **Not** the same as
    /// [`Self::Passthrough`]: "failed to parse" is not "safe to release
    /// unexamined" — a missing or unparseable attribute is "unknown", and
    /// unknown routes to the strictest branch (see the crate's fail-closed
    /// house rule). The caller must scan the raw line as opaque text before
    /// releasing it, exactly as it would scan `content`. A malformed frame
    /// is not a coincidence a hostile or misbehaving upstream cannot
    /// engineer on purpose to smuggle a credential past a parser that only
    /// ever looks inside successfully-parsed JSON.
    Opaque,
}

/// Classifies one raw SSE line (including its trailing newline, or none at
/// end-of-stream). `[DONE]` and every non-`data:` line (blank lines,
/// `event:`, `id:`, `retry:`, comments) pass through untouched. A `data:`
/// line whose payload fails to parse as JSON is [`ClassifiedLine::Opaque`],
/// never [`ClassifiedLine::Passthrough`] — see that variant's doc for why.
pub(crate) fn classify_line(raw: &str) -> ClassifiedLine {
    let trimmed = raw.trim_end_matches(['\n', '\r']);
    let payload = trimmed.strip_prefix("data:").map(str::trim);
    match payload {
        Some(p) if p != DONE && !p.is_empty() => {
            serde_json::from_str::<Value>(p).map_or(ClassifiedLine::Opaque, ClassifiedLine::Data)
        }
        _ => ClassifiedLine::Passthrough,
    }
}

/// The result of redacting a stream.
#[derive(Debug, Clone)]
pub struct StreamOutcome {
    /// The rewritten SSE body, ready to send. Empty when blocked.
    pub body: String,
    /// What the scan found.
    pub report: ScanReport,
}

/// Scans and rewrites a complete SSE completion stream.
///
/// `body` is the entire upstream response body. Returns the rewritten stream,
/// or a [`ScanReport`] whose [`ScanReport::is_blocked`] is true if the assistant
/// text contained something that must not be forwarded — in which case
/// [`StreamOutcome::body`] is empty and the caller must send an error instead.
///
/// # Errors
///
/// Propagates [`crate::Error`] from the engine. On a `fail_closed` profile the
/// caller must reject rather than forward the original stream.
pub fn scan_sse(engine: &Engine, body: &str) -> Result<StreamOutcome> {
    let mut chunks: Vec<Value> = Vec::new();
    let mut lines: Vec<Line> = Vec::new();
    let mut report = ScanReport::default();

    // Split on '\n' and keep every line, so the re-emitted body has the same
    // shape as the original. SSE frames are separated by blank lines, which
    // land in `Passthrough` and are reproduced.
    for raw in body.split_inclusive('\n') {
        match classify_line(raw) {
            ClassifiedLine::Data(parsed) => {
                chunks.push(parsed);
                lines.push(Line::Data(chunks.len().saturating_sub(1)));
            }
            // Not a `data:` line at all (blank, `event:`, `id:`, `retry:`,
            // `data: [DONE]`). Pass it through rather than dropping it — a
            // provider extension is not our business.
            ClassifiedLine::Passthrough => lines.push(Line::Passthrough(raw.to_string())),
            // A `data:` payload that failed to parse as JSON. Unlike
            // `Passthrough`, this is scanned as opaque text before release —
            // see `ClassifiedLine::Opaque`'s doc for why "did not parse" must
            // not mean "did not look".
            ClassifiedLine::Opaque => {
                let rewritten = match report.merge_verdict(engine.scan(raw)?) {
                    Some(new) => new,
                    None => raw.to_string(),
                };
                lines.push(Line::Passthrough(rewritten));
            }
        }
    }

    // Coalesce the assistant text per choice/field. Two streams interleaved
    // over `n > 1`, or a choice's `content` and its tool calls' arguments,
    // must not have their text concatenated together, or the redacted
    // output would be written back to the wrong slot.
    let mut coalesced: HashMap<ContentKey, String> = HashMap::new();
    for chunk in &chunks {
        for (key, content) in delta_contents(chunk) {
            coalesced.entry(key).or_default().push_str(&content);
        }
    }

    let mut redacted: HashMap<ContentKey, String> = HashMap::new();

    for (key, text) in &coalesced {
        if text.is_empty() {
            continue;
        }
        match report.merge_verdict(engine.scan(text)?) {
            Some(new) => {
                redacted.insert(*key, new);
            }
            None => {
                // Clean, or blocked. Either way nothing to write back for this
                // slot; a block is detected via the report below.
            }
        }
    }

    if report.is_blocked() {
        return Ok(StreamOutcome {
            body: String::new(),
            report,
        });
    }

    // Write the redacted text back onto the first chunk that carried each
    // choice/field, and blank the rest.
    let mut written: HashMap<ContentKey, bool> = HashMap::new();
    for chunk in &mut chunks {
        rewrite_deltas(chunk, &redacted, &mut written);
    }

    let mut out = String::with_capacity(body.len());
    for line in lines {
        match line {
            Line::Passthrough(raw) => out.push_str(&raw),
            Line::Data(i) => {
                let rendered = chunks
                    .get(i)
                    .map(|c| serde_json::to_string(c).unwrap_or_default())
                    .unwrap_or_default();
                out.push_str("data: ");
                out.push_str(&rendered);
                out.push('\n');
            }
        }
    }

    Ok(StreamOutcome { body: out, report })
}

/// Extracts `(ContentKey, text)` pairs from one chunk: each choice's
/// `content`, plus each of its tool calls' `function.arguments`.
///
/// `pub(crate)`: [`crate::sse::SseHoldBack`] needs the identical extraction
/// rule the buffered path uses, so the two never disagree about what counts
/// as redactable content in a chunk.
pub(crate) fn delta_contents(chunk: &Value) -> Vec<(ContentKey, String)> {
    let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for choice in choices {
        let index = choice.get("index").and_then(Value::as_u64).unwrap_or(0);
        let Some(delta) = choice.get("delta") else {
            continue;
        };

        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            out.push(((index, ContentField::Content), content.to_string()));
        }

        // Tool-call arguments are model-authored JSON strings that routinely
        // echo user input straight back — see the module doc and
        // `crate::payload::scan_message`'s identical buffered-path rule.
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in tool_calls {
                let call_index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                let Some(args) = call
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                out.push((
                    (index, ContentField::ToolCallArguments(call_index)),
                    args.to_string(),
                ));
            }
        }
    }
    out
}

/// Locates the mutable JSON slot one [`ContentField`] refers to within a
/// single choice object (`choice`, not `chunk` — the caller has already
/// found the matching choice by index).
fn field_slot_mut(choice: &mut Value, field: ContentField) -> Option<&mut Value> {
    match field {
        ContentField::Content => choice.get_mut("delta")?.get_mut("content"),
        ContentField::ToolCallArguments(call_index) => {
            let calls = choice
                .get_mut("delta")?
                .get_mut("tool_calls")?
                .as_array_mut()?;
            let call = calls
                .iter_mut()
                .find(|c| c.get("index").and_then(Value::as_u64).unwrap_or(0) == call_index)?;
            call.get_mut("function")?.get_mut("arguments")
        }
    }
}

/// Writes redacted text back onto a chunk's deltas.
///
/// The first chunk carrying a given choice/field receives the whole redacted
/// string; subsequent ones are blanked, so concatenating deltas reproduces
/// the redacted text exactly once.
fn rewrite_deltas(
    chunk: &mut Value,
    redacted: &HashMap<ContentKey, String>,
    written: &mut HashMap<ContentKey, bool>,
) {
    let Some(choices) = chunk.get_mut("choices").and_then(Value::as_array_mut) else {
        return;
    };
    for choice in choices.iter_mut() {
        let index = choice.get("index").and_then(Value::as_u64).unwrap_or(0);
        // A chunk may carry both `content` and one or more tool calls, so
        // every key belonging to this choice is a candidate, not just one.
        let keys: Vec<ContentKey> = redacted
            .keys()
            .copied()
            .filter(|(choice_index, _)| *choice_index == index)
            .collect();
        for key in keys {
            let Some(replacement) = redacted.get(&key) else {
                continue;
            };
            let Some(slot) = field_slot_mut(choice, key.1) else {
                continue;
            };
            if !slot.is_string() {
                continue;
            }
            if written.get(&key).copied().unwrap_or(false) {
                *slot = Value::String(String::new());
            } else {
                *slot = Value::String(replacement.clone());
                written.insert(key, true);
            }
        }
    }
}

/// Sets the text at one [`ContentKey`] slot within a chunk, if that slot
/// exists and is a string. Returns whether it did anything.
///
/// `pub(crate)`: unlike [`rewrite_deltas`], which resolves every choice
/// across a whole buffered stream in one pass, [`crate::sse::SseHoldBack`]
/// resolves one choice/field's contribution to one frame at a time, as the
/// incremental redactor decides each frame is safe to release.
pub(crate) fn set_delta_content(chunk: &mut Value, key: ContentKey, text: &str) -> bool {
    let (index, field) = key;
    let Some(choices) = chunk.get_mut("choices").and_then(Value::as_array_mut) else {
        return false;
    };
    for choice in choices.iter_mut() {
        let choice_index = choice.get("index").and_then(Value::as_u64).unwrap_or(0);
        if choice_index != index {
            continue;
        }
        let Some(slot) = field_slot_mut(choice, field) else {
            return false;
        };
        if !slot.is_string() {
            return false;
        }
        *slot = Value::String(text.to_string());
        return true;
    }
    false
}

impl ScanReport {
    /// Folds a verdict into this report, returning replacement text if any.
    ///
    /// Mirrors the private helper used for non-streaming bodies; exposed within
    /// the crate so the streaming path reports identically.
    pub(crate) fn merge_verdict(&mut self, verdict: crate::engine::Verdict) -> Option<String> {
        self.scanned_fields += 1;
        match verdict {
            crate::engine::Verdict::Clean => None,
            crate::engine::Verdict::Redacted { text, count } => {
                self.redactions += count;
                Some(text)
            }
            crate::engine::Verdict::Blocked { entities } => {
                self.blocked.extend(entities);
                self.blocked.sort_unstable();
                self.blocked.dedup();
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::scan_sse;
    use crate::{engine::Engine, profile::Profile};

    fn engine() -> Engine {
        Engine::new(Profile::coding_assistant(), "salt").expect("engine")
    }

    fn sse(chunks: &[&str]) -> String {
        let mut s = String::new();
        for c in chunks {
            s.push_str("data: ");
            s.push_str(c);
            s.push_str("\n\n");
        }
        s.push_str("data: [DONE]\n\n");
        s
    }

    /// Concatenates delta content the way any real client does.
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

    #[test]
    fn clean_stream_round_trips_its_text() {
        let e = engine();
        let body = sse(&[
            r#"{"choices":[{"index":0,"delta":{"content":"let x"}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":" = 1;"}}]}"#,
        ]);
        let out = scan_sse(&e, &body).expect("scan");
        assert_eq!(concat_deltas(&out.body), "let x = 1;");
        assert_eq!(out.report.redactions, 0);
    }

    #[test]
    fn done_sentinel_is_preserved() {
        let e = engine();
        let body = sse(&[r#"{"choices":[{"index":0,"delta":{"content":"hi"}}]}"#]);
        let out = scan_sse(&e, &body).expect("scan");
        assert!(
            out.body.contains("data: [DONE]"),
            "stream must still terminate"
        );
    }

    #[test]
    fn entity_split_across_chunks_is_still_caught() {
        // THE reason buffered mode exists. Neither chunk contains a full
        // address; only the coalesced text does.
        let e = engine();
        let body = sse(&[
            r#"{"choices":[{"index":0,"delta":{"content":"mail jane@ex"}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":"ample.com now"}}]}"#,
        ]);
        let out = scan_sse(&e, &body).expect("scan");
        let text = concat_deltas(&out.body);
        assert!(
            !text.contains("jane@example.com"),
            "split entity survived: {text}"
        );
        assert_eq!(out.report.redactions, 1);
    }

    #[test]
    fn credential_split_across_chunks_blocks_the_stream() {
        let e = engine();
        let body = sse(&[
            r#"{"choices":[{"index":0,"delta":{"content":"ghp_abcdefghijkl"}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":"mnopqrstuvwxyz0123456789"}}]}"#,
        ]);
        let out = scan_sse(&e, &body).expect("scan");
        assert!(out.report.is_blocked(), "split credential must block");
        assert!(out.body.is_empty(), "blocked stream must emit nothing");
    }

    #[test]
    fn non_content_fields_survive() {
        let e = engine();
        let body = sse(&[
            r#"{"id":"c1","model":"glm","choices":[{"index":0,"delta":{"role":"assistant"}}]}"#,
            r#"{"id":"c1","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}]}"#,
            r#"{"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"total_tokens":5}}"#,
        ]);
        let out = scan_sse(&e, &body).expect("scan");
        assert!(out.body.contains("\"role\":\"assistant\""));
        assert!(out.body.contains("\"finish_reason\":\"stop\""));
        assert!(out.body.contains("\"total_tokens\":5"));
        assert!(out.body.contains("\"model\":\"glm\""));
    }

    #[test]
    fn multiple_choices_do_not_bleed_into_each_other() {
        // n > 1: choice 0 is clean, choice 1 has an address. Coalescing them
        // together would redact the wrong stream.
        let e = engine();
        let body = sse(&[
            r#"{"choices":[{"index":0,"delta":{"content":"all fine here"}},{"index":1,"delta":{"content":"write jane@example.com"}}]}"#,
        ]);
        let out = scan_sse(&e, &body).expect("scan");
        assert!(out.body.contains("all fine here"), "clean choice altered");
        assert!(
            !out.body.contains("jane@example.com"),
            "dirty choice not redacted"
        );
    }

    #[test]
    fn malformed_data_line_is_passed_through_not_dropped() {
        let e = engine();
        let body = "data: not json at all\n\ndata: [DONE]\n\n";
        let out = scan_sse(&e, body).expect("scan");
        assert!(out.body.contains("not json at all"));
        assert!(out.body.contains("[DONE]"));
    }

    #[test]
    fn empty_stream_is_handled() {
        let e = engine();
        let out = scan_sse(&e, "").expect("scan");
        assert!(out.body.is_empty());
        assert_eq!(out.report.redactions, 0);
    }

    #[test]
    fn redacted_text_appears_exactly_once() {
        // The redistribution rule: first chunk gets the whole string, the rest
        // are blanked. Concatenation must not duplicate it.
        let e = engine();
        let body = sse(&[
            r#"{"choices":[{"index":0,"delta":{"content":"a@b.com"}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":" and more"}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":" text"}}]}"#,
        ]);
        let out = scan_sse(&e, &body).expect("scan");
        let text = concat_deltas(&out.body);
        assert!(!text.contains("a@b.com"));
        assert_eq!(
            text.matches("and more").count(),
            1,
            "content duplicated on redistribution: {text}"
        );
    }

    // ── Tool-call arguments: the P0 this module used to miss entirely. ─────
    // `delta_contents` extracted only `delta.content`; `delta.tool_calls[].
    // function.arguments` was invisible to the scanner, so a credential
    // riding in a tool call rode straight through as "no redactable
    // content". These tests would fail against that code for exactly that
    // reason: `report.is_blocked()` false and the secret present verbatim
    // in `out.body`.

    #[test]
    fn tool_call_arguments_with_credential_block_the_stream() {
        let e = engine();
        let body = sse(&[
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ghp_abcdefghijklmnopqrstuvwxyz0123456789"}}]}}]}"#,
        ]);
        let out = scan_sse(&e, &body).expect("scan");
        assert!(
            out.report.is_blocked(),
            "credential in tool_call arguments must block: {:?}",
            out.report
        );
        assert!(out.body.is_empty(), "blocked stream must emit nothing");
    }

    #[test]
    fn tool_call_arguments_pii_is_redacted_not_leaked() {
        let e = engine();
        let body = sse(&[
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"to\":\"jane@example.com\"}"}}]}}]}"#,
        ]);
        let out = scan_sse(&e, &body).expect("scan");
        assert_eq!(out.report.redactions, 1);
        assert!(
            !out.body.contains("jane@example.com"),
            "tool_call argument leaked: {}",
            out.body
        );
    }

    #[test]
    fn tool_call_arguments_and_content_are_independent_streams() {
        // Redacting one must not touch the other -- they are different
        // ContentField keys under the same choice index.
        let e = engine();
        let body = sse(&[
            r#"{"choices":[{"index":0,"delta":{"content":"all fine here","tool_calls":[{"index":0,"function":{"arguments":"mail jane@example.com"}}]}}]}"#,
        ]);
        let out = scan_sse(&e, &body).expect("scan");
        assert!(
            out.body.contains("all fine here"),
            "clean content wrongly altered: {}",
            out.body
        );
        assert!(
            !out.body.contains("jane@example.com"),
            "tool_call argument leaked: {}",
            out.body
        );
    }

    // ── Malformed `data:` payloads: must be scanned, never released
    //    unexamined. A parse failure is "unknown", and unknown routes to
    //    the strictest branch. ─────────────────────────────────────────────

    #[test]
    fn malformed_data_line_with_secret_is_not_leaked() {
        let e = engine();
        // Missing the final closing brace: invalid JSON, but the credential
        // inside is plainly there for a text scan to find.
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"ghp_abcdefghijklmnopqrstuvwxyz0123456789\"}}]\n\ndata: [DONE]\n\n";
        let out = scan_sse(&e, body).expect("scan");
        assert!(
            out.report.is_blocked(),
            "credential in malformed data frame must block: {:?}",
            out.report
        );
        assert!(out.body.is_empty(), "blocked stream must emit nothing");
    }
}
