//! GitHub Copilot connector (RFC-0001): a PULL connector.
//!
//! Polls GitHub's daily aggregated Copilot reports, follows their short-lived
//! signed download URLs, and hands the raw NDJSON to the caller for S3 archive
//! and Postgres upsert (RFC-0001). The fetching/auth/parsing live here; the
//! persistence (upsert + `ingest_manifests`) lives in the collector CLI and
//! governance-core.
//!
//! ⚠️ Access requires the org's "Copilot usage metrics" policy toggle enabled
//! (an org SETTING, not a permission) plus an App with `Organization Copilot
//! metrics: Read` (+ `Copilot seat management: Read` for seats, `Metadata:
//! Read`). Spike-0007 proved `Members: Read` is NOT required -- the reports
//! endpoints return 200 without it. Until the toggle is on, an otherwise
//! correct App still gets `403` "policy must be enabled". See RFC-0001 §2.

/// The org-scoped report endpoints this connector ingests.
///
/// All take `?day=YYYY-MM-DD`. Reports exist from 2025-10-10 and stay
/// available for roughly one year.
pub const REPORTS: &[&str] = &[
    "organization-1-day",
    "users-1-day",
    "repos-1-day",
    // Org-scope team membership. GitHub omits teams with fewer than five seated
    // users -- documented caveat, not a mapping table.
    "user-teams-1-day",
];

/// Pinned REST API version. Sent as `X-GitHub-Api-Version` on every request.
pub const API_VERSION: &str = "2026-03-10";

/// Bumped whenever the normalized copilot table shape changes, so a `replay`
/// over an older S3 archive can detect drift (RFC-0001 verification).
pub const SCHEMA_VERSION: i64 = 1;

mod auth;
mod client;
mod error;
mod model;
mod parse;
mod report;
mod secret;
mod store;
mod sync;

pub use auth::AppAuth;
pub use client::GithubClient;
pub use error::{CopilotError, Result};
pub use model::{
    OrgDaily, OrgReportRow, RepoDaily, RepoReportRow, ReportEnvelope, UserDaily, UserReportRow,
    UserTeam, UserTeamRow,
};
pub use parse::{
    credits_to_micro_usd, parse_org_daily, parse_repo_daily, parse_user_daily, parse_user_team,
};
pub use report::DownloadedReport;
pub use secret::RawSecret;
pub use store::{
    ManifestDrift, high_water_mark, manifest_schema_version, unmapped_user_count, upsert_manifest,
    upsert_org_daily, upsert_repo_daily, upsert_user_daily, upsert_user_team, verify_manifests,
};
pub use sync::{ReportOutcome, archive_key, replay_report, sync_day};
