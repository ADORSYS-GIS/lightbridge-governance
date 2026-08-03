//! Tenant -> Application -> Environment -> Integration registry (ADR-0005).
//!
//! Built BEFORE either connector: both need `tenant_id` on every row, and
//! retrofitting it means rewriting every primary key.
//!
//! Single-tenant per deployment (ADR-0001) -- `tenant_id` exists so a customer
//! install and ours share one schema, not so one install serves many customers.

/// Where a piece of telemetry or usage data came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// GitHub Copilot daily report API (RFC-0001).
    GithubCopilot,
    /// Microsoft Foundry hosted agents over OTLP (RFC-0002).
    MicrosoftFoundry,
    /// Claude Code CLI with native OTLP exporter (#30, #32).
    ClaudeCode,
    /// OpenAI Codex CLI with native OTLP exporter (#30, #33).
    Codex,
}

impl Provider {
    /// The wire string the schema stores in `integrations.provider` and the
    /// collector sends in the `governance.source` header.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GithubCopilot => "github_copilot",
            Self::MicrosoftFoundry => "microsoft_foundry",
            Self::ClaudeCode => "claude_code",
            Self::Codex => "codex",
        }
    }
}

impl std::str::FromStr for Provider {
    /// Dedicated parse error: parsing a wire string is a pure string concern
    /// and must not couple the domain [`crate::Error`] (which carries
    /// persistence details) to a string parser.
    type Err = ProviderParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "github_copilot" => Ok(Self::GithubCopilot),
            "microsoft_foundry" => Ok(Self::MicrosoftFoundry),
            "claude_code" => Ok(Self::ClaudeCode),
            "codex" => Ok(Self::Codex),
            other => Err(ProviderParseError::Unknown(other.to_owned())),
        }
    }
}

/// A provider string that does not match any known provider.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderParseError {
    /// The wire string is not a recognized provider.
    #[error("unknown provider: {0}")]
    Unknown(String),
}

/// How much of a request/response body an integration is allowed to retain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentCapture {
    /// Identifiers, counts and timings only. The default (RFC-0002 Increment 4).
    #[default]
    MetadataOnly,
    /// Content retained after secret/PII redaction.
    Redacted,
    /// Full content. Requires explicit opt-in and shortened retention.
    Full,
}

#[cfg(test)]
mod tests {
    use super::{ContentCapture, Provider};

    #[test]
    fn provider_serializes_as_snake_case() {
        let json = serde_json::to_string(&Provider::GithubCopilot).expect("serialize provider");
        assert_eq!(json, "\"github_copilot\"");
    }

    #[test]
    fn content_capture_defaults_to_metadata_only() {
        assert_eq!(ContentCapture::default(), ContentCapture::MetadataOnly);
    }
}
