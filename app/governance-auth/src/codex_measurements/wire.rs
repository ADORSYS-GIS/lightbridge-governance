use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::parse::Fact;

pub(super) fn envelope(facts: &[&Fact]) -> Result<Value> {
    // This is a new measurement of existing turn metadata, not a replay of
    // Codex's original event. Preserve original lifecycle timestamps as fields.
    // Loki rejects late source timestamps beyond its out-of-order window.
    let observed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("clock precedes Unix epoch; refusing timestamp-free measurements")?
        .as_nanos();
    let records: Vec<_> = facts
        .iter()
        .map(|f| {
            let mut attributes = vec![
                text("event.name", "governance.codex.turn"),
                text("conversation.id", &f.session),
                text("turn.id", &f.turn),
                text("turn.status", &f.status),
                text("measurement.version", "codex-desktop-0.154.0-alpha.6.2-v2"),
                integer("duration_ms", f.duration_ms),
                integer("started_at", f.started),
                integer("completed_at", f.ended),
            ];
            if let Some(repository) = &f.repository {
                attributes.push(text("repository", repository));
                attributes.push(text(
                    "repository.basis",
                    "launch-metadata-matching-turn-cwd",
                ));
            }
            json!({"timeUnixNano": observed.to_string(),
            "severityText": "INFO", "body": {"stringValue": "governance.codex.turn"},
            "attributes": attributes})
        })
        .collect();
    Ok(json!({"resourceLogs": [{"resource": {"attributes": [
        text("service.name", "governance-codex-session")
    ]}, "scopeLogs": [{"scope": {"name": "governance.codex.measurements"}, "logRecords": records}]}]}))
}

fn text(key: &str, value: &str) -> Value {
    json!({"key": key, "value": {"stringValue": value}})
}
fn integer(key: &str, value: u64) -> Value {
    json!({"key": key, "value": {"intValue": value.to_string()}})
}
