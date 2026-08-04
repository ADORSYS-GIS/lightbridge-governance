//! Typed structure of the Copilot report NDJSON as GitHub publishes it, and
//! the normalized rows we persist (RFC-0001, source spec §5).
//!
//! GitHub keeps the exact NDJSON field names under-documented, so the mapping
//! is deliberately:
//!   - tolerant: every numeric field is `#[serde(default)]` and unknown fields
//!     are ignored, so a field GitHub renames or adds never wedges ingestion;
//!   - centralised: the serde structs below ARE the only place field names
//!     appear. When the first live download shows a real payload differs from
//!     this (it likely will in small ways), the correction is confined to this
//!     module, not scattered through parse/upsert code.
//!
//! Money is integer micro-USD via `governance_core::MicroUsd` (ADR-0008). The
//! raw NDJSON is archived to S3 before parsing, so a mapping bug is replayed,
//! not refetched (RFC-0001).

use std::collections::HashMap;

use governance_core::MicroUsd;
use serde::Deserialize;

/// One row of the `organization-1-day` report.
#[derive(Debug, Clone, Deserialize)]
pub struct OrgReportRow {
    #[serde(rename = "day")]
    pub day: String,
    #[serde(rename = "organization_id", default)]
    pub organization_id: Option<String>,
    #[serde(rename = "total_active_users", default)]
    pub total_active_users: Option<u64>,
    #[serde(rename = "total_engaged_users", default)]
    pub total_engaged_users: Option<u64>,
    #[serde(rename = "total_completions", default)]
    pub total_completions: Option<u64>,
    #[serde(rename = "total_chat_engagements", default)]
    pub total_chat_engagements: Option<u64>,
    #[serde(flatten)]
    pub totals_by_feature: HashMap<String, serde_json::Value>,
}

/// One row of the `users-1-day` report.
#[derive(Debug, Clone, Deserialize)]
pub struct UserReportRow {
    #[serde(rename = "user_id", default)]
    pub user_id: Option<String>,
    #[serde(rename = "user_login", default)]
    pub user_login: Option<String>,
    #[serde(rename = "day")]
    pub day: String,
    #[serde(rename = "total_engagements", default)]
    pub total_engagements: Option<u64>,
    #[serde(rename = "total_completions", default)]
    pub total_completions: Option<u64>,
    #[serde(default)]
    pub ai_credits: Option<f64>,
}

/// One row of the `repos-1-day` report.
#[derive(Debug, Clone, Deserialize)]
pub struct RepoReportRow {
    #[serde(rename = "repo_id", alias = "repository_id", default)]
    pub repo_id: Option<String>,
    #[serde(rename = "repo_name", alias = "name", default)]
    pub repo_name: Option<String>,
    #[serde(rename = "day")]
    pub day: String,
    #[serde(rename = "coding_agent_activity", default)]
    pub coding_agent_activity: Option<u64>,
    #[serde(rename = "code_review_activity", default)]
    pub code_review_activity: Option<u64>,
    #[serde(rename = "pull_request_activity", default)]
    pub pull_request_activity: Option<u64>,
}

/// One row of the `user-teams-1-day` report.
#[derive(Debug, Clone, Deserialize)]
pub struct UserTeamRow {
    #[serde(rename = "user_id", default)]
    pub user_id: Option<String>,
    #[serde(rename = "user_login", default)]
    pub user_login: Option<String>,
    #[serde(rename = "team_id", default)]
    pub team_id: Option<String>,
    #[serde(rename = "slug", default)]
    pub slug: Option<String>,
    #[serde(rename = "day")]
    pub day: String,
}

/// The daily report envelope returned by the report endpoints. The NDJSON
/// payload itself lives behind the signed download URLs.
#[derive(Debug, Clone, Deserialize)]
pub struct ReportEnvelope {
    #[serde(rename = "download_links", default)]
    pub download_links: Vec<String>,
    #[serde(rename = "report_day", default)]
    pub report_day: Option<String>,
}

impl ReportEnvelope {
    /// The single download link this org-scope report envelopes (docs return
    /// one per report). We never print it — only its host (RFC-0001 risk #2).
    pub fn download_url(&self) -> Option<&str> {
        self.download_links.first().map(String::as_str)
    }
}

/// A normalized row for `copilot_org_dailys`.
#[derive(Debug, Clone)]
pub struct OrgDaily {
    pub organization_id: String,
    pub report_day: String,
    pub active_users: u64,
    pub engaged_users: u64,
    pub total_interactions: u64,
    pub total_completions: u64,
    pub ai_credits: u64,
    pub net_cost_micro_usd: MicroUsd,
}

/// A normalized row for `copilot_user_dailys`.
#[derive(Debug, Clone)]
pub struct UserDaily {
    pub provider_user_id: String,
    pub user_login: String,
    pub report_day: String,
    pub total_interactions: u64,
    pub total_completions: u64,
    pub ai_credits: u64,
    pub net_cost_micro_usd: MicroUsd,
}

/// A normalized row for `copilot_user_teams`.
#[derive(Debug, Clone)]
pub struct UserTeam {
    pub user_id: String,
    pub team_id: String,
    pub team_slug: String,
    pub report_day: String,
}

/// A normalized row for `copilot_repo_dailys`.
#[derive(Debug, Clone)]
pub struct RepoDaily {
    pub repository_id: String,
    pub report_day: String,
    pub coding_agent_activity: u64,
    pub code_review_activity: u64,
    pub pull_request_activity: u64,
}
