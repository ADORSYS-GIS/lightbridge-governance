//! GitHub Copilot connector (RFC-0001): a PULL connector.
//!
//! Polls GitHub's daily aggregated Copilot reports, follows their short-lived
//! signed download URLs, archives the raw NDJSON to S3 and upserts the normalized
//! rows into Postgres.
//!
//! ⚠️ Access requires THREE org permissions -- Copilot metrics, Copilot seat
//! management AND Members (all read) -- plus the organization's "Copilot metrics
//! API access policy" toggle, which is an org SETTING, not a permission. An app
//! with every box ticked still gets 403 until it is enabled. See RFC-0001 §2.

/// The org-scoped report endpoints this connector ingests.
///
/// All take `?day=YYYY-MM-DD` except the `*-28-day/latest` pair. Reports exist
/// from 2025-10-10 and stay available for roughly one year.
pub const REPORTS: &[&str] = &[
    "organization-1-day",
    "users-1-day",
    "repos-1-day",
    // Org-scope team membership. The source spec claimed this was enterprise-only;
    // it is not, which is why there is no manual GitHub-login -> team mapping table.
    // Caveat that IS real: GitHub omits teams with fewer than five seated users.
    "user-teams-1-day",
];

/// Pinned REST API version. Sent as `X-GitHub-Api-Version` on every request.
pub const API_VERSION: &str = "2026-03-10";
