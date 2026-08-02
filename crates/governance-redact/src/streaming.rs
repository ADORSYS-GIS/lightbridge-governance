//! Redacting a Server-Sent Events completion stream.
//!
//! Streaming is the hard case, and on this platform it is also the *normal*
//! one: opencode, Kilo-Code and LibreChat all stream by default, so a redactor
//! that only handles buffered responses cannot go in front of real traffic.
//!
//! # Buffered, not incremental
//!
//! This module implements **buffered** redaction: consume the whole upstream
//! stream, coalesce the assistant text, scan it once, then re-emit. That gives
//! a property incremental redaction cannot — detection sees the complete text,
//! so **no entity can hide in a token split**. A credential streamed as
//! `ghp_` + `abc…` is one string by the time we look at it.
//!
//! The cost is real and worth stating: time-to-first-token becomes
//! time-to-*last*-token, because nothing is emitted until the upstream stream
//! finishes. An incremental mode with a hold-back window trades that latency
//! back for a weaker guarantee (entities longer than the window can still
//! straddle it). Buffered is the safe default; incremental is a later, opt-in
//! addition.
//!
//! # What is preserved
//!
//! Chunk **sequence** and every field on every chunk. The only values rewritten
//! are `choices[i].delta.content`. Role announcements, `finish_reason`,
//! `usage`, `system_fingerprint` and unknown provider extensions pass through
//! untouched, because a client parsing them must still find what it expects.
//!
//! Because the text is coalesced for detection and then written back, the
//! redacted string lands on the **first** chunk that carried text for a choice
//! and later fragments become empty strings. Clients therefore receive fewer
//! non-empty content deltas than the provider sent — inherent to buffering, and
//! invisible to any client that concatenates deltas (which is all of them).

use std::collections::HashMap;

use serde_json::Value;

use crate::{engine::Engine, error::Result, payload::ScanReport};

/// The sentinel terminating an OpenAI SSE stream.
const DONE: &str = "[DONE]";

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

    // Split on '\n' and keep every line, so the re-emitted body has the same
    // shape as the original. SSE frames are separated by blank lines, which
    // land in `Passthrough` and are reproduced.
    for raw in body.split_inclusive('\n') {
        let trimmed = raw.trim_end_matches(['\n', '\r']);
        let payload = trimmed.strip_prefix("data:").map(str::trim);

        match payload {
            Some(p) if p != DONE && !p.is_empty() => {
                if let Ok(parsed) = serde_json::from_str::<Value>(p) {
                    chunks.push(parsed);
                    lines.push(Line::Data(chunks.len().saturating_sub(1)));
                } else {
                    // Not JSON we understand. Pass it through rather than
                    // dropping it — a provider extension is not our business.
                    lines.push(Line::Passthrough(raw.to_string()));
                }
            }
            _ => lines.push(Line::Passthrough(raw.to_string())),
        }
    }

    // Coalesce the assistant text per choice index. Two streams interleaved
    // over `n > 1` must not have their text concatenated together, or the
    // redacted output would be written back to the wrong choice.
    let mut coalesced: HashMap<u64, String> = HashMap::new();
    for chunk in &chunks {
        for (index, content) in delta_contents(chunk) {
            coalesced.entry(index).or_default().push_str(&content);
        }
    }

    let mut report = ScanReport::default();
    let mut redacted: HashMap<u64, String> = HashMap::new();

    for (index, text) in &coalesced {
        if text.is_empty() {
            continue;
        }
        match report.merge_verdict(engine.scan(text)?) {
            Some(new) => {
                redacted.insert(*index, new);
            }
            None => {
                // Clean, or blocked. Either way nothing to write back for this
                // choice; a block is detected via the report below.
            }
        }
    }

    if report.is_blocked() {
        return Ok(StreamOutcome {
            body: String::new(),
            report,
        });
    }

    // Write the redacted text back onto the first chunk that carried content
    // for each choice, and blank the rest.
    let mut written: HashMap<u64, bool> = HashMap::new();
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

/// Extracts `(choice_index, content)` pairs from one chunk.
fn delta_contents(chunk: &Value) -> Vec<(u64, String)> {
    let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
        return Vec::new();
    };
    choices
        .iter()
        .filter_map(|choice| {
            let index = choice.get("index").and_then(Value::as_u64).unwrap_or(0);
            let content = choice
                .get("delta")
                .and_then(|d| d.get("content"))
                .and_then(Value::as_str)?;
            Some((index, content.to_string()))
        })
        .collect()
}

/// Writes redacted text back onto a chunk's deltas.
///
/// The first chunk carrying content for a choice receives the whole redacted
/// string; subsequent ones are blanked, so concatenating deltas reproduces the
/// redacted text exactly once.
fn rewrite_deltas(
    chunk: &mut Value,
    redacted: &HashMap<u64, String>,
    written: &mut HashMap<u64, bool>,
) {
    let Some(choices) = chunk.get_mut("choices").and_then(Value::as_array_mut) else {
        return;
    };
    for choice in choices.iter_mut() {
        let index = choice.get("index").and_then(Value::as_u64).unwrap_or(0);
        let Some(replacement) = redacted.get(&index) else {
            continue;
        };
        let Some(slot) = choice.get_mut("delta").and_then(|d| d.get_mut("content")) else {
            continue;
        };
        if !slot.is_string() {
            continue;
        }
        if written.get(&index).copied().unwrap_or(false) {
            *slot = Value::String(String::new());
        } else {
            *slot = Value::String(replacement.clone());
            written.insert(index, true);
        }
    }
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
}
