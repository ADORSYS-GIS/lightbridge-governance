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

use chrono::{DateTime, Utc};
use governance_core::MicroUsd;
use serde::Deserialize;

/// GitHub's Copilot report NDJSON is inconsistent about whether an id field
/// (`repo_id`, `user_id`, `organization_id`, `team_id`) is a JSON string or a
/// JSON number. Observed live 2026-08-07: `repos-1-day` sends `repo_id` as a
/// bare integer (e.g. `844522530`), not the string every other report's id
/// fields have been seen as -- and every id field in this module was typed
/// `Option<String>` on that same (wrong) assumption, so any of them can hit
/// the identical "invalid type: integer, expected a string" the moment a
/// report with non-empty rows exercises it (only `repos-1-day` had any that
/// day; the others were legitimately empty, not fixed by luck). Applied to
/// every id field below rather than patched once for `repo_id` alone.
fn flexible_id<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum IdValue {
        String(String),
        Number(i64),
    }
    Ok(
        Option::<IdValue>::deserialize(deserializer)?.map(|v| match v {
            IdValue::String(s) => s,
            IdValue::Number(n) => n.to_string(),
        }),
    )
}

/// One row of the `organization-1-day` report.
#[derive(Debug, Clone, Deserialize)]
pub struct OrgReportRow {
    #[serde(rename = "day")]
    pub day: String,
    #[serde(rename = "organization_id", default, deserialize_with = "flexible_id")]
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
    #[serde(rename = "user_id", default, deserialize_with = "flexible_id")]
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
    #[serde(
        rename = "repo_id",
        alias = "repository_id",
        default,
        deserialize_with = "flexible_id"
    )]
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
    #[serde(rename = "user_id", default, deserialize_with = "flexible_id")]
    pub user_id: Option<String>,
    #[serde(rename = "user_login", default)]
    pub user_login: Option<String>,
    #[serde(rename = "team_id", default, deserialize_with = "flexible_id")]
    pub team_id: Option<String>,
    #[serde(rename = "slug", default)]
    pub slug: Option<String>,
    #[serde(rename = "day")]
    pub day: String,
}

/// One page of `/orgs/{org}/copilot/billing/seats`. GitHub wraps each
/// page's seat list in an envelope alongside a `total_seats` count; we only
/// need the list, so a sibling field GitHub adds or renames is ignored, not
/// fatal (matching this module's declared tolerance policy).
#[derive(Debug, Clone, Deserialize)]
pub struct SeatsPage {
    #[serde(rename = "seats", default)]
    pub seats: Vec<SeatRow>,
}

/// One assigned Copilot seat, as GitHub's billing/seats endpoint returns
/// it. GitHub gives no explicit lifecycle/"state" field here -- see
/// `parse::seat_state` for how `SeatSnapshot::seat_state` is derived from
/// `pending_cancellation_date`.
#[derive(Debug, Clone, Deserialize)]
pub struct SeatRow {
    #[serde(rename = "created_at", default)]
    pub created_at: Option<String>,
    #[serde(rename = "last_activity_at", default)]
    pub last_activity_at: Option<String>,
    #[serde(rename = "last_activity_editor", default)]
    pub last_activity_editor: Option<String>,
    #[serde(rename = "pending_cancellation_date", default)]
    pub pending_cancellation_date: Option<String>,
    #[serde(rename = "assignee", default)]
    pub assignee: Option<SeatAssignee>,
}

/// The user a seat is currently assigned to.
#[derive(Debug, Clone, Deserialize)]
pub struct SeatAssignee {
    #[serde(rename = "id", default, deserialize_with = "flexible_id")]
    pub id: Option<String>,
    #[serde(rename = "login", default)]
    pub login: Option<String>,
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

/// A normalized row for `copilot_seat_snapshots`. Unlike the daily reports'
/// `report_day` (a plain "YYYY-MM-DD" the request itself supplied),
/// `seat_assigned_at`/`last_activity_at` are timestamps GitHub reports
/// per-seat, so they are parsed into real `DateTime<Utc>` here rather than
/// carried as opaque strings -- the column is `timestamptz`, and only a
/// typed value round-trips through `sqlx` without a second parse at the
/// call site (see `store::upsert_seat_snapshot`).
#[derive(Debug, Clone)]
pub struct SeatSnapshot {
    pub provider_user_id: String,
    pub user_login: String,
    /// Always today's date, stamped by the caller (`sync::sync_seats`) --
    /// GitHub's seats endpoint has no `day` parameter and no history to ask
    /// for one.
    pub snapshot_day: String,
    pub seat_assigned_at: Option<DateTime<Utc>>,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub last_activity_editor: Option<String>,
    /// `"active"` or `"pending_cancellation"` -- see `parse::seat_state`.
    pub seat_state: String,
}
