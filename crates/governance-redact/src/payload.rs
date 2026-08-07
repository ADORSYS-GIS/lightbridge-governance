//! Walking OpenAI-shaped request and response bodies.
//!
//! The engine works on strings; this module decides *which* strings in an
//! OpenAI JSON body are user content worth scanning, and rewrites them in
//! place.
//!
//! # Why an explicit path list, and not "scan every string"
//!
//! Scanning every string in the body would redact `model`, `role`, `id`,
//! `finish_reason` and every field name-shaped value in the payload — plenty of
//! which match `Hostname` or `Uuid`. It would also rewrite fields the provider
//! parses structurally, producing a request that is no longer valid. The
//! trade-off is that a provider extension carrying user text in a field we do
//! not know about is missed; that is the safer direction to be wrong in, and it
//! is why [`ScanReport::scanned_fields`] is reported rather than assumed.
//!
//! # Role in the architecture
//!
//! This module is the *buffered* scanning path. The request body (and non-
//! streaming response body) is always available in full before any byte is
//! forwarded, so one synchronous walk is sufficient. The buffer is safe by
//! nature: because the entire body is present before any action is taken, no
//! entity can hide across a field boundary or a chunk boundary.
//!
//! See the crate-level docs ([`crate`]) for the full two-path architecture
//! (buffered request, incremental streaming response) and why each path has
//! the shape it does.

use serde_json::Value;

use crate::{engine::Engine, error::Result};

/// What a scan of a whole body found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScanReport {
    /// Entity types that triggered a block, deduplicated. Non-empty means the
    /// body must not be forwarded.
    pub blocked: Vec<String>,
    /// How many spans were rewritten across the whole body.
    pub redactions: usize,
    /// How many individual text fields were examined. Zero on a body whose
    /// shape we did not recognise — worth alerting on, since it means the
    /// request passed through uninspected.
    pub scanned_fields: usize,
}

impl ScanReport {
    /// Whether the body must be rejected rather than forwarded.
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        !self.blocked.is_empty()
    }
}

/// Scans and rewrites a request body in place.
///
/// Covers the OpenAI request surfaces that carry user text: chat `messages`
/// (string content and `text` content-parts), `input` (embeddings, string or
/// array), `prompt` (legacy completions, string or array), and tool-call
/// `arguments` on assistant messages.
///
/// # Errors
///
/// Propagates [`crate::Error`] from the engine. On a `fail_closed` profile the
/// caller must reject the request on any error rather than forward it.
pub fn scan_request(engine: &Engine, body: &mut Value) -> Result<ScanReport> {
    let mut report = ScanReport::default();

    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages.iter_mut() {
            scan_message(engine, message, &mut report)?;
        }
    }

    for key in ["input", "prompt"] {
        if let Some(field) = body.get_mut(key) {
            scan_string_or_array(engine, field, &mut report)?;
        }
    }

    Ok(report)
}

/// Scans and rewrites a non-streaming response body in place.
///
/// Covers `choices[].message.content` (chat) and `choices[].text` (legacy
/// completions).
///
/// # Errors
///
/// Propagates [`crate::Error`] from the engine.
pub fn scan_response(engine: &Engine, body: &mut Value) -> Result<ScanReport> {
    let mut report = ScanReport::default();

    if let Some(choices) = body.get_mut("choices").and_then(Value::as_array_mut) {
        for choice in choices.iter_mut() {
            if let Some(message) = choice.get_mut("message") {
                scan_message(engine, message, &mut report)?;
            }
            if let Some(text) = choice.get_mut("text") {
                scan_string_or_array(engine, text, &mut report)?;
            }
        }
    }

    Ok(report)
}

/// Scans one JSON slot, rewriting it in place if it holds a string.
///
/// The single place any text is actually handed to the engine, so every
/// caller below is just "which slots count as user content".
///
/// A non-string slot is a no-op rather than an error: `content` is legitimately
/// `null` on a tool-call-only assistant message, and a number or object there
/// is a provider extension we do not scan.
fn scan_slot(engine: &Engine, slot: &mut Value, report: &mut ScanReport) -> Result<()> {
    let Some(text) = slot.as_str().map(str::to_owned) else {
        return Ok(());
    };
    if let Some(new) = report.merge_verdict(engine.scan(&text)?) {
        *slot = Value::String(new);
    }
    Ok(())
}

/// Scans one chat message object: its `content` and any tool-call arguments.
fn scan_message(engine: &Engine, message: &mut Value, report: &mut ScanReport) -> Result<()> {
    if let Some(content) = message.get_mut("content") {
        scan_content(engine, content, report)?;
    }

    // Tool-call arguments are model-authored JSON strings that routinely echo
    // user input straight back, so they are as much a leak surface as content.
    if let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) {
        for call in calls.iter_mut() {
            if let Some(slot) = call
                .get_mut("function")
                .and_then(|f| f.get_mut("arguments"))
            {
                scan_slot(engine, slot, report)?;
            }
        }
    }

    Ok(())
}

/// Scans a `content` field, which is either a string or an array of parts.
fn scan_content(engine: &Engine, content: &mut Value, report: &mut ScanReport) -> Result<()> {
    match content {
        Value::Array(parts) => {
            for part in parts.iter_mut() {
                // Multimodal parts: only `text` carries scannable content.
                // Image/audio parts are deliberately left alone.
                if let Some(slot) = part.get_mut("text") {
                    scan_slot(engine, slot, report)?;
                }
            }
            Ok(())
        }
        other => scan_slot(engine, other, report),
    }
}

/// Scans a field that may be a bare string or an array of strings.
fn scan_string_or_array(engine: &Engine, field: &mut Value, report: &mut ScanReport) -> Result<()> {
    match field {
        Value::Array(items) => {
            for item in items.iter_mut() {
                scan_slot(engine, item, report)?;
            }
            Ok(())
        }
        other => scan_slot(engine, other, report),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{scan_request, scan_response};
    use crate::{engine::Engine, profile::Profile};

    fn engine() -> Engine {
        Engine::new(Profile::coding_assistant(), "salt").expect("engine")
    }

    const TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

    #[test]
    fn redacts_chat_message_content() {
        let e = engine();
        let mut body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "mail jane@example.com"}]
        });
        let report = scan_request(&e, &mut body).expect("scan");
        assert_eq!(report.redactions, 1);
        let content = body["messages"][0]["content"].as_str().expect("content");
        assert!(!content.contains("jane@example.com"), "leaked: {content}");
    }

    #[test]
    fn blocks_on_credential_in_content() {
        let e = engine();
        let mut body = json!({
            "messages": [{"role": "user", "content": format!("token {TOKEN}")}]
        });
        let report = scan_request(&e, &mut body).expect("scan");
        assert!(report.is_blocked(), "credential must block: {report:?}");
    }

    #[test]
    fn does_not_rewrite_structural_fields() {
        // `model` and `role` must survive verbatim or the request stops being
        // valid for the provider.
        let e = engine();
        let mut body = json!({
            "model": "glm-4.7-flash",
            "messages": [{"role": "user", "content": "hello"}]
        });
        scan_request(&e, &mut body).expect("scan");
        assert_eq!(body["model"], "glm-4.7-flash");
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn redacts_multimodal_text_parts_only() {
        let e = engine();
        let mut body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "reach me at jane@example.com"},
                    {"type": "image_url", "image_url": {"url": "https://example.com/a.png"}}
                ]
            }]
        });
        let report = scan_request(&e, &mut body).expect("scan");
        assert_eq!(report.redactions, 1);
        let text = body["messages"][0]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert!(!text.contains("jane@example.com"));
        // The image part is untouched.
        assert_eq!(
            body["messages"][0]["content"][1]["image_url"]["url"],
            "https://example.com/a.png"
        );
    }

    #[test]
    fn redacts_embeddings_input_string_and_array() {
        let e = engine();
        let mut body = json!({"model": "emb", "input": "jane@example.com"});
        let report = scan_request(&e, &mut body).expect("scan");
        assert_eq!(report.redactions, 1);

        let mut body = json!({"model": "emb", "input": ["a@b.com", "plain text"]});
        let report = scan_request(&e, &mut body).expect("scan");
        assert_eq!(report.redactions, 1);
        assert_eq!(body["input"][1], "plain text");
    }

    #[test]
    fn redacts_tool_call_arguments() {
        // Model-authored tool arguments echo user input back; they leak too.
        let e = engine();
        let mut body = json!({
            "messages": [{
                "role": "assistant",
                "tool_calls": [{
                    "function": {"name": "send", "arguments": "{\"to\":\"jane@example.com\"}"}
                }]
            }]
        });
        let report = scan_request(&e, &mut body).expect("scan");
        assert_eq!(report.redactions, 1);
        let args = body["messages"][0]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .expect("arguments");
        assert!(!args.contains("jane@example.com"), "leaked: {args}");
    }

    #[test]
    fn redacts_response_choices() {
        let e = engine();
        let mut body = json!({
            "choices": [{"message": {"role": "assistant", "content": "it is jane@example.com"}}]
        });
        let report = scan_response(&e, &mut body).expect("scan");
        assert_eq!(report.redactions, 1);
        let content = body["choices"][0]["message"]["content"]
            .as_str()
            .expect("content");
        assert!(!content.contains("jane@example.com"));
    }

    #[test]
    fn blocks_on_credential_in_response() {
        // The model echoing a key back is just as much a leak as sending one.
        let e = engine();
        let mut body = json!({
            "choices": [{"message": {"content": format!("here: {TOKEN}")}}]
        });
        let report = scan_response(&e, &mut body).expect("scan");
        assert!(report.is_blocked());
    }

    #[test]
    fn unrecognised_body_reports_zero_scanned_fields() {
        // The signal that a request went through uninspected. Callers should
        // treat this as alertable rather than as "clean".
        let e = engine();
        let mut body = json!({"some_future_provider_field": "jane@example.com"});
        let report = scan_request(&e, &mut body).expect("scan");
        assert_eq!(report.scanned_fields, 0);
        assert_eq!(report.redactions, 0);
        assert!(!report.is_blocked());
    }

    #[test]
    fn clean_body_is_byte_identical() {
        let e = engine();
        let original = json!({
            "model": "glm-4.7-flash",
            "messages": [{"role": "user", "content": "fn main() { let x = 1; }"}]
        });
        let mut body = original.clone();
        let report = scan_request(&e, &mut body).expect("scan");
        assert_eq!(report.redactions, 0);
        assert_eq!(body, original, "clean traffic must pass through unchanged");
    }
}
