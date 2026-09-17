//! Input types, deterministic id derivation, and validation for the telemetry
//! ingest pipeline.

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::{Error, Result};

/// Normalized telemetry from a push connector.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecutionInput {
    pub trace_id: String,
    pub span_id: String,
    pub user_email: Option<String>,
    pub started_at: DateTime<Utc>,
    pub duration_ms: i64,
    pub model_calls: Vec<ModelCallInput>,
    pub tool_calls: Vec<ToolCallInput>,
}

/// One LLM call within an execution.
///
/// `input_tokens`/`output_tokens` are optional: a provider may omit token
/// counts, in which case the call is still recorded but its cost is stored as
/// *unknown* (`None`), never a zero -- a zero is indistinguishable from "free"
/// on a dashboard (story #31 AC6).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelCallInput {
    pub trace_id: String,
    pub span_id: String,
    pub model: String,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

/// One tool invocation within an execution.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCallInput {
    pub trace_id: String,
    pub span_id: String,
    pub tool_name: String,
    pub duration_ms: i64,
}

/// Result of an ingest operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IngestResult {
    pub executions_upserted: i64,
    pub model_calls_upserted: i64,
    pub tool_calls_upserted: i64,
    /// Whether identity mismatch detection query failed (best-effort).
    pub identity_mismatch_detection_failed: bool,
    /// Executions whose payload email contradicted the token-derived identity.
    pub identity_mismatches: i64,
}

/// Derives a deterministic id from `(trace_id, span_id)` so the same
/// execution always maps to the same row -- critical for idempotent upsert,
/// since child rows (model_calls, tool_calls) reference this id as their FK.
pub(crate) fn deterministic_id(prefix: &str, trace_id: &str, span_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(trace_id.as_bytes());
    hasher.update(b":");
    hasher.update(span_id.as_bytes());
    let hash = hex::encode(hasher.finalize());
    format!("{prefix}-{}", &hash[..24])
}

/// Validates caller-supplied telemetry before any persistence happens.
///
/// The idempotency key is `(trace_id, span_id)`; every row must carry a
/// non-empty one or the upsert's `ON CONFLICT` has nothing reliable to target.
/// Token counts and durations must be non-negative (a negative token count is
/// malformed input, not a "zero-cost call").
pub(crate) fn validate_input(executions: &[ExecutionInput]) -> Result<()> {
    let reject = |message: String| Err(Error::Validation(message));

    for execution in executions {
        if execution.trace_id.is_empty() || execution.span_id.is_empty() {
            return reject("trace_id and span_id are required on every execution".to_owned());
        }
        if execution.duration_ms < 0 {
            return reject(format!(
                "duration_ms must be non-negative, got {}",
                execution.duration_ms
            ));
        }
        for model_call in &execution.model_calls {
            if model_call.trace_id.is_empty() || model_call.span_id.is_empty() {
                return reject("trace_id and span_id are required on every model call".to_owned());
            }
            // A missing token count is "unknown", which is stored as unknown
            // cost -- not malformed. A *negative* count is malformed input,
            // not a zero-cost call.
            if model_call.input_tokens.is_some_and(|n| n < 0)
                || model_call.output_tokens.is_some_and(|n| n < 0)
            {
                return reject(format!(
                    "token counts must be non-negative, got input={:?} output={:?} for model {}",
                    model_call.input_tokens, model_call.output_tokens, model_call.model
                ));
            }
        }
        for tool_call in &execution.tool_calls {
            if tool_call.trace_id.is_empty() || tool_call.span_id.is_empty() {
                return reject("trace_id and span_id are required on every tool call".to_owned());
            }
            if tool_call.duration_ms < 0 {
                return reject(format!(
                    "tool duration_ms must be non-negative, got {}",
                    tool_call.duration_ms
                ));
            }
        }
    }
    Ok(())
}
