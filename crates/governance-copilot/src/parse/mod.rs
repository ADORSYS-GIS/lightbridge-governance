//! Pure, unit-testable parsing of Copilot report NDJSON into normalized rows.
//!
//! These functions take raw `&[u8]` and return typed rows or a parse error.
//! They never touch the network and never print a payload body, which is what
//! makes them safe to unit-test against recorded fixtures and safe to call in
//! production without leaking a signed URL.
//!
//! Money: the report may give AI credits (a float). We convert to integer
//! micro-USD here, once, using integer arithmetic (including the rounding) so
//! no float ever lands in a stored monetary value (ADR-0008).

use chrono::{DateTime, Utc};
use governance_core::MicroUsd;

use crate::{
    error::{CopilotError, Result},
    model::{
        OrgDaily, OrgReportRow, RepoDaily, RepoReportRow, SeatSnapshot, SeatsPage, UserDaily,
        UserReportRow, UserTeam, UserTeamRow,
    },
};

/// AI credits per micro-USD: 1 AI credit = 1 cent = 10_000 micro-USD.
const CREDIT_MICRO_USD: u64 = 10_000;

/// Convert a float of AI credits to integer micro-USD, rounding half up. Used
/// only at the boundary between the report payload and the stored row; the
/// stored row carries a `MicroUsd` i64 (ADR-0008).
pub fn credits_to_micro_usd(credits: f64) -> MicroUsd {
    MicroUsd((credits * CREDIT_MICRO_USD as f64).round() as i64)
}

/// Parse `organization-1-day` NDJSON into normalized daily rows.
pub fn parse_org_daily(bytes: &[u8], report: &str, day: &str) -> Result<Vec<OrgDaily>> {
    let mut out = Vec::new();
    for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let row: OrgReportRow =
            serde_json::from_slice(line).map_err(|source| CopilotError::Parse {
                report: report.to_owned(),
                day: day.to_owned(),
                source,
            })?;
        // Aggregate row carries the org totals; skip rows with no org id rather
        // than fabricate one.
        let organization_id = match row.organization_id {
            Some(id) if !id.is_empty() => id,
            _ => continue,
        };
        out.push(OrgDaily {
            organization_id,
            report_day: row.day,
            active_users: row.total_active_users.unwrap_or(0),
            engaged_users: row.total_engaged_users.unwrap_or(0),
            total_interactions: row
                .total_completions
                .unwrap_or(0)
                .saturating_add(row.total_chat_engagements.unwrap_or(0)),
            total_completions: row.total_completions.unwrap_or(0),
            ai_credits: 0,
            net_cost_micro_usd: MicroUsd(0),
        });
    }
    Ok(out)
}

/// Parse `users-1-day` NDJSON into normalized per-user rows.
pub fn parse_user_daily(bytes: &[u8], report: &str, day: &str) -> Result<Vec<UserDaily>> {
    let mut out = Vec::new();
    for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let row: UserReportRow =
            serde_json::from_slice(line).map_err(|source| CopilotError::Parse {
                report: report.to_owned(),
                day: day.to_owned(),
                source,
            })?;
        let user_id = match row.user_id {
            Some(id) if !id.is_empty() => id,
            _ => continue,
        };
        let ai_credits = row.ai_credits.unwrap_or(0.0);
        // Stored credits are derived from the exact money value (rounded down
        // to whole credits), so the two columns can never disagree in origin.
        // GitHub credits are fractional in practice (a 2.5-credit day is in
        // the fixture below): money is the record of truth (ADR-0008), the
        // credit count is a whole-credit approximation for display.
        let cost = credits_to_micro_usd(ai_credits);
        out.push(UserDaily {
            provider_user_id: user_id,
            user_login: row.user_login.unwrap_or_default(),
            report_day: row.day,
            total_interactions: row.total_engagements.unwrap_or(0),
            total_completions: row.total_completions.unwrap_or(0),
            ai_credits: (cost.0 / CREDIT_MICRO_USD as i64) as u64,
            net_cost_micro_usd: cost,
        });
    }
    Ok(out)
}

/// Parse `repos-1-day` NDJSON into normalized per-repo rows.
pub fn parse_repo_daily(bytes: &[u8], report: &str, day: &str) -> Result<Vec<RepoDaily>> {
    let mut out = Vec::new();
    for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let row: RepoReportRow =
            serde_json::from_slice(line).map_err(|source| CopilotError::Parse {
                report: report.to_owned(),
                day: day.to_owned(),
                source,
            })?;
        let repo_id = match (row.repo_id.as_ref(), row.repo_name.as_ref()) {
            (Some(id), _) if !id.is_empty() => id.clone(),
            (_, Some(name)) if !name.is_empty() => name.clone(),
            _ => continue,
        };
        out.push(RepoDaily {
            repository_id: repo_id,
            report_day: row.day,
            coding_agent_activity: row.coding_agent_activity.unwrap_or(0),
            code_review_activity: row.code_review_activity.unwrap_or(0),
            pull_request_activity: row.pull_request_activity.unwrap_or(0),
        });
    }
    Ok(out)
}

/// Parse `user-teams-1-day` NDJSON into normalized user-team rows.
pub fn parse_user_team(bytes: &[u8], report: &str, day: &str) -> Result<Vec<UserTeam>> {
    let mut out = Vec::new();
    for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let row: UserTeamRow =
            serde_json::from_slice(line).map_err(|source| CopilotError::Parse {
                report: report.to_owned(),
                day: day.to_owned(),
                source,
            })?;
        let user_id = match row.user_id {
            Some(id) if !id.is_empty() => id,
            _ => continue,
        };
        let team_id = match row.team_id {
            Some(id) if !id.is_empty() => id,
            _ => continue,
        };
        out.push(UserTeam {
            user_id,
            team_id,
            team_slug: row.slug.unwrap_or_default(),
            report_day: row.day,
        });
    }
    Ok(out)
}

/// GitHub's seats endpoint has no explicit lifecycle field: a seat only
/// appears in the listing while it is assigned, so every listed seat is, by
/// definition, currently active. The one forward-looking signal it does
/// carry is `pending_cancellation_date`: non-null means the seat is
/// assigned today but will not renew at the next billing cycle. We surface
/// that distinction as its own state rather than collapsing every listed
/// seat into a single `"active"` value, because "assigned but scheduled to
/// leave" is exactly the kind of signal RFC-0001's motivating question
/// ("who has a seat and has never used it") wants visible without a second
/// join against billing data this connector does not ingest.
fn seat_state(pending_cancellation_date: Option<&str>) -> &'static str {
    if pending_cancellation_date.is_some() {
        "pending_cancellation"
    } else {
        "active"
    }
}

/// Parse a raw RFC 3339 timestamp as GitHub sends it (e.g.
/// `"2021-08-03T18:00:00-06:00"`). Absent or unparseable becomes `None` --
/// unknown, never a fabricated zero time -- so a future format change
/// degrades one field to "we don't know when", not a hard failure of the
/// whole seat snapshot.
fn parse_seat_timestamp(raw: Option<&str>) -> Option<DateTime<Utc>> {
    raw.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// Parse an archived seat listing (see `crate::seats::FetchedSeats::
/// to_archive_bytes`: a JSON array of the raw per-page bodies) into
/// normalized seat-snapshot rows, stamped with `snapshot_day`. A row whose
/// assignee has no resolvable id is skipped rather than fabricated,
/// matching every other `parse_*` function's id handling here.
pub fn parse_seats(bytes: &[u8], report: &str, snapshot_day: &str) -> Result<Vec<SeatSnapshot>> {
    let pages: Vec<SeatsPage> =
        serde_json::from_slice(bytes).map_err(|source| CopilotError::Parse {
            report: report.to_owned(),
            day: snapshot_day.to_owned(),
            source,
        })?;
    let mut out = Vec::new();
    for page in pages {
        for seat in page.seats {
            let Some(assignee) = seat.assignee else {
                continue;
            };
            let provider_user_id = match assignee.id {
                Some(id) if !id.is_empty() => id,
                _ => continue,
            };
            out.push(SeatSnapshot {
                provider_user_id,
                user_login: assignee.login.unwrap_or_default(),
                snapshot_day: snapshot_day.to_owned(),
                seat_assigned_at: parse_seat_timestamp(seat.created_at.as_deref()),
                last_activity_at: parse_seat_timestamp(seat.last_activity_at.as_deref()),
                last_activity_editor: seat.last_activity_editor,
                seat_state: seat_state(seat.pending_cancellation_date.as_deref()).to_owned(),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
