//! Normalizer trait and dispatch for push connectors (#30).
//!
//! Each provider (Claude Code, Codex, Foundry) has its own OTLP span format.
//! The normalizer trait converts provider-specific spans into the normalized
//! `ExecutionInput` model that the ingest endpoint expects.
//!
//! Dispatch is **data-driven, keyed by provider string** (story #31 AC1/AC4):
//! the ingest endpoint hands the provider string from the registered
//! integration's row to [`dispatch_normalizer`], which looks it up in a static
//! map. The ingest path contains no provider enum and no match over providers
//! -- the list lives in exactly one place, this module's `NORMALIZERS` table.
//! Adding a provider is a new normalizer plus one map entry; nothing in the
//! auth, quota or ingest paths changes.

use std::{collections::HashMap, sync::LazyLock};

use governance_core::ingest::ExecutionInput;

pub mod claude_code;
pub mod codex;
pub mod foundry;
pub mod otlp;

/// Normalized telemetry from a push connector.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TelemetryPayload {
    pub executions: Vec<ExecutionInput>,
}

/// Normalizer trait: converts provider-specific OTLP spans into the normalized model.
///
/// Each provider emits different span attributes and event structures. The normalizer
/// extracts the relevant fields and maps them to `ExecutionInput`, `ModelCallInput`,
/// and `ToolCallInput`.
///
/// `Send + Sync`: normalizers are dispatched from an async HTTP handler, so the
/// trait object must be safe to hold across an await.
///
/// # Errors
///
/// Returns an error if the telemetry is malformed or missing required fields.
/// The caller should reject the request and log the error for investigation.
pub trait Normalizer: Send + Sync {
    /// Normalizes provider-specific telemetry into the unified model.
    ///
    /// # Errors
    ///
    /// Returns an error if the telemetry cannot be normalized.
    fn normalize(&self, payload: &serde_json::Value) -> Result<TelemetryPayload, NormalizerError>;
}

/// Errors from normalization.
#[derive(Debug, thiserror::Error)]
pub enum NormalizerError {
    #[error("missing required field: {0}")]
    MissingField(String),

    #[error("invalid field type: {field} expected {expected}, got {actual}")]
    InvalidFieldType {
        field: String,
        expected: String,
        actual: String,
    },

    /// `endTimeUnixNano - startTimeUnixNano` either overflowed `i64` or came
    /// out negative. Both are producer-controlled and unbounded (proto3 JSON
    /// int64-as-string carries no sign restriction), so this is malformed
    /// input, not a duration to clamp to zero -- the strictest branch per the
    /// house rule on unparseable data.
    #[error(
        "invalid span duration: end {end_time_unix_nano} is not after start \
         {start_time_unix_nano} (or the subtraction overflowed)"
    )]
    InvalidDuration {
        start_time_unix_nano: i64,
        end_time_unix_nano: i64,
    },

    #[error("unsupported provider: {0}")]
    UnsupportedProvider(String),
}

/// The registration table: provider wire string -> normalizer.
///
/// This is the single seam for adding a provider (story #31 AC4): write a
/// normalizer and add one entry here. The ingest endpoint only ever calls
/// [`dispatch_normalizer`] with the integration row's provider string; it does
/// not enumerate this list.
static NORMALIZERS: LazyLock<HashMap<&'static str, &'static dyn Normalizer>> =
    LazyLock::new(|| {
        // The array element type annotation drives the unsized coercion, so no
        // explicit `as &dyn Normalizer` cast is needed here.
        let entries: [(&'static str, &'static dyn Normalizer); 3] = [
            ("claude_code", &claude_code::ClaudeCodeNormalizer),
            ("codex", &codex::CodexNormalizer),
            ("microsoft_foundry", &foundry::FoundryNormalizer),
        ];
        HashMap::from(entries)
    });

/// Dispatches telemetry to the normalizer registered for `provider`.
///
/// Takes the provider as a plain string (the integration row's data) rather
/// than an enum, so the request path never needs to know the provider list.
/// Returns a `&'static` reference: every normalizer is a zero-sized struct
/// with no state, so there is exactly one instance for the life of the process
/// -- no per-request allocation on the hot ingest path.
///
/// # Errors
///
/// Returns [`NormalizerError::UnsupportedProvider`] if no normalizer is
/// registered for the provider string.
pub fn dispatch_normalizer(provider: &str) -> Result<&'static dyn Normalizer, NormalizerError> {
    NORMALIZERS
        .get(provider)
        .copied()
        .ok_or_else(|| NormalizerError::UnsupportedProvider(provider.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_is_string_keyed_and_returns_the_registered_normalizer() {
        // Dispatch is data-driven by provider string -- the ingest path calls
        // this with the integration row's provider, not a provider enum (story
        // #31 AC1). Every registered provider resolves.
        for provider in ["claude_code", "codex", "microsoft_foundry"] {
            assert!(
                dispatch_normalizer(provider).is_ok(),
                "registered provider {provider} must dispatch"
            );
        }
    }

    #[test]
    fn dispatch_rejects_an_unregistered_provider() {
        // A provider string with no normalizer is a hard rejection, never a
        // silent unnormalized store (story #31 AC5).
        let result = dispatch_normalizer("github_copilot");
        assert!(matches!(
            result,
            Err(NormalizerError::UnsupportedProvider(_))
        ));
        assert!(matches!(
            dispatch_normalizer("brand_new_provider"),
            Err(NormalizerError::UnsupportedProvider(_))
        ));
    }

    #[test]
    fn an_added_provider_requires_no_dispatch_consumer_change() {
        // The structural proof of AC4: dispatch consumes a plain `&str`, so
        // the request path has nothing to edit when a provider is added --
        // there is no enum to extend and no match to update. An arbitrary
        // string flows through the same code path as every registered one; the
        // only difference is whether a normalizer is registered for it.
        let first = dispatch_normalizer("codex").expect("registered provider resolves");
        let arbitrary = dispatch_normalizer("future_provider_x");
        assert!(arbitrary.is_err(), "unregistered stays rejected");
        assert!(first.normalize(&serde_json::json!({})).is_err());
    }
}
