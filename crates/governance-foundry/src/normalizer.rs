//! Normalizer trait and dispatch for push connectors (#30).
//!
//! Each provider (Claude Code, Codex, Foundry) has its own OTLP span format.
//! The normalizer trait converts provider-specific spans into the normalized
//! `ExecutionInput` model that the ingest endpoint expects.
//!
//! The dispatch function routes incoming telemetry to the correct normalizer
//! based on the `provider` field from the authenticated integration credential.

use governance_core::{ingest::ExecutionInput, registry::Provider};

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

    #[error("unsupported provider: {0}")]
    UnsupportedProvider(String),
}

/// Dispatches telemetry to the correct normalizer based on provider.
///
/// Returns a `&'static` reference: every normalizer is a zero-sized struct
/// with no state, so there is exactly one instance that can be handed out for
/// the life of the process -- no per-request allocation on the hot ingest
/// path.
///
/// # Errors
///
/// Returns an error if the provider is not supported or normalization fails.
pub fn dispatch_normalizer(provider: Provider) -> Result<&'static dyn Normalizer, NormalizerError> {
    match provider {
        Provider::ClaudeCode => Ok(&claude_code::ClaudeCodeNormalizer),
        Provider::Codex => Ok(&codex::CodexNormalizer),
        Provider::MicrosoftFoundry => Ok(&foundry::FoundryNormalizer),
        Provider::GithubCopilot => Err(NormalizerError::UnsupportedProvider(
            provider.as_str().to_owned(),
        )),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_returns_claude_code_normalizer() {
        let normalizer = dispatch_normalizer(Provider::ClaudeCode);
        assert!(normalizer.is_ok());
    }

    #[test]
    fn dispatch_returns_codex_normalizer() {
        let normalizer = dispatch_normalizer(Provider::Codex);
        assert!(normalizer.is_ok());
    }

    #[test]
    fn dispatch_returns_foundry_normalizer() {
        let normalizer = dispatch_normalizer(Provider::MicrosoftFoundry);
        assert!(normalizer.is_ok());
    }

    #[test]
    fn dispatch_rejects_unsupported_provider() {
        let result = dispatch_normalizer(Provider::GithubCopilot);
        assert!(matches!(
            result,
            Err(NormalizerError::UnsupportedProvider(_))
        ));
    }
}
