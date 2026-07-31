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
