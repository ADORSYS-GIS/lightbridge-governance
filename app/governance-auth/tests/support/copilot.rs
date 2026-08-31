//! Fixtures for the `copilot-push` tests: a synthetic spool, the two session
//! shapes, and the one way these tests invoke the command.
//!
//! The spool content is **synthetic**. The parser was validated against a real
//! Copilot spool on a developer machine, but that file carries `session.id`
//! and per-session model and tool detail, so nothing from it is committed --
//! the shapes below reproduce it with the identifying values replaced.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::harness::Harness;

/// The default file name `copilot::spool::DEFAULT_FILE_NAME` compiles in.
/// Re-stated rather than imported because `tests/` cannot reach `src/`; a
/// drift here shows up as the spool-path test failing, which is the point.
pub const DEFAULT_SPOOL_FILE_NAME: &str = "copilot-otel.jsonl";

/// Mirrors `copilot::checkpoint::FILE_NAME`, for the same reason.
pub const CHECKPOINT_FILE_NAME: &str = "copilot-push.json";

pub fn checkpoint_path(harness: &Harness) -> PathBuf {
    harness.state_dir().join(CHECKPOINT_FILE_NAME)
}

/// Writes a two-record spool (one metrics line, one log line) at a path the
/// test then passes explicitly, and returns it.
pub fn seed_spool(harness: &Harness) -> Result<PathBuf> {
    let dir = harness.state_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating the state dir {}", dir.display()))?;
    let path = dir.join(DEFAULT_SPOOL_FILE_NAME);
    write_spool(&path, &[metrics_line(), log_line()])?;
    Ok(path)
}

pub fn write_spool(path: &Path, lines: &[Value]) -> Result<()> {
    let mut contents = String::new();
    for line in lines {
        contents.push_str(&line.to_string());
        contents.push('\n');
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

/// The one way these tests invoke the command, so a flag added in one test
/// cannot silently differ from the others.
pub async fn push(
    harness: &Harness,
    collector: &str,
    spool: &Path,
    extra: &[&str],
) -> Result<std::process::Output> {
    let spool = spool.display().to_string();
    let mut args = vec![
        "copilot-push",
        "--otel-endpoint",
        collector,
        "--copilot-spool-path",
        &spool,
    ];
    args.extend_from_slice(extra);
    harness.run(&args).await
}

fn now_unix() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_secs())
}

/// A session valid for another hour, so nothing here ever needs the issuer to
/// be reachable -- these tests are about the drain, not about refresh.
pub fn fresh_session(issuer: &str) -> Result<Value> {
    Ok(json!({
        "issuer": issuer,
        "client_id": "test-client",
        "access_token": "valid-access-token",
        "refresh_token": null,
        "expires_at": now_unix()?.saturating_add(3600),
    }))
}

pub fn expired_session(issuer: &str) -> Result<Value> {
    Ok(json!({
        "issuer": issuer,
        "client_id": "test-client",
        "access_token": "expired-access-token",
        "refresh_token": null,
        "expires_at": now_unix()?.saturating_sub(3600),
    }))
}

pub fn metrics_line() -> Value {
    json!({
        "resource": {
            "_rawAttributes": [
                ["service.name", "copilot-chat"],
                ["service.version", "0.62.0"],
                ["session.id", "00000000-0000-0000-0000-000000000000"],
            ],
            "_asyncAttributesPending": false,
        },
        "scopeMetrics": [{
            "scope": { "name": "copilot-chat", "version": "0.62.0" },
            "metrics": [{
                "descriptor": {
                    "name": "copilot_chat.session.count",
                    "type": "COUNTER",
                    "description": "",
                    "unit": "",
                    "valueType": 1,
                    "advice": {},
                },
                "aggregationTemporality": 1,
                "dataPointType": 3,
                "dataPoints": [{
                    "attributes": {},
                    "startTime": [1788191912, 133000000],
                    "endTime": [1788191916, 86000000],
                    "value": 1,
                }],
                "isMonotonic": true,
            }],
        }],
    })
}

/// A log record as it would look after a Copilot release renamed the private
/// fields this parser dispatches on (`_body` -> `body`, `hrTime` -> `time`).
/// Neither key is present, so `classify` cannot place it: this is exactly the
/// drift `record.rs`'s module doc anticipates, and the whole point is that it
/// must not vanish quietly.
pub fn drifted_line() -> Value {
    json!({
        "time": [1788191912, 613000000],
        "instrumentationScope": { "name": "copilot-chat", "version": "0.62.0" },
        "resource": { "_rawAttributes": [["service.name", "copilot-chat"]] },
        "attributes": { "event.name": "copilot_chat.tool.call" },
        "body": "copilot_chat.tool.call: manage_todo_list",
    })
}

/// A well-formed log record carrying `marker` in its body, so a mock collector
/// can reject exactly the batches that contain it.
pub fn marked_log_line(marker: &str) -> Value {
    let mut line = log_line();
    if let Some(object) = line.as_object_mut() {
        object.insert("_body".to_owned(), Value::String(marker.to_owned()));
    }
    line
}

pub fn log_line() -> Value {
    json!({
        "hrTime": [1788191912, 613000000],
        "hrTimeObserved": [1788191912, 613000000],
        "spanContext": {
            "traceId": "0123456789abcdef0123456789abcdef",
            "spanId": "0123456789abcdef",
            "traceFlags": 1,
        },
        "instrumentationScope": { "name": "copilot-chat", "version": "0.62.0" },
        "resource": { "_rawAttributes": [["service.name", "copilot-chat"]] },
        "attributes": {
            "event.name": "copilot_chat.tool.call",
            "gen_ai.tool.name": "manage_todo_list",
            "success": true,
        },
        "_body": "copilot_chat.tool.call: manage_todo_list",
        "totalAttributesCount": 3,
        "_isReadonly": true,
    })
}
