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

use governance_core::MicroUsd;

use crate::{
    error::{CopilotError, Result},
    model::{
        OrgDaily, OrgReportRow, RepoDaily, RepoReportRow, UserDaily, UserReportRow, UserTeam,
        UserTeamRow,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credits_to_micro_usd_uses_integer_math() {
        // 1 AI credit = 1 cent = 10_000 micro-USD.
        assert_eq!(credits_to_micro_usd(1.0), MicroUsd(10_000));
        // 12.5 credits = 125_000 micro-USD; rounding of halves is integer not float.
        assert_eq!(credits_to_micro_usd(12.5), MicroUsd(125_000));
    }

    #[test]
    fn parse_org_daily_extracts_aggregates_and_skips_bare_lines() {
        let ndjson = concat!(
            "{\"day\":\"2026-08-01\",\"organization_id\":\"g1\",",
            "\"total_active_users\":10,\"total_engaged_users\":4,",
            "\"total_completions\":120,\"total_chat_engagements\":30}\n",
            // A row without an org id is skipped, not fabricated.
            "{\"day\":\"2026-08-01\",\"total_completions\":5}\n",
        );
        let rows = parse_org_daily(ndjson.as_bytes(), "organization-1-day", "2026-08-01").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].organization_id, "g1");
        assert_eq!(rows[0].active_users, 10);
        assert_eq!(rows[0].engaged_users, 4);
        assert_eq!(rows[0].total_interactions, 150); // 120 + 30
        assert_eq!(rows[0].total_completions, 120);
    }

    #[test]
    fn parse_user_daily_converts_credits_to_micro_usd() {
        let ndjson = concat!(
            "{\"day\":\"2026-08-01\",\"user_id\":\"1001\",\"user_login\":\"octocat\",",
            "\"total_engagements\":42,\"total_completions\":20,\"ai_credits\":2.5}\n",
        );
        let rows = parse_user_daily(ndjson.as_bytes(), "users-1-day", "2026-08-01").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider_user_id, "1001");
        assert_eq!(rows[0].user_login, "octocat");
        assert_eq!(rows[0].total_interactions, 42);
        // 2.5 credits: money is exact (2.5 * 10k), the stored credit count is
        // derived from that money value (25000 / 10000 = 2), so the two
        // columns always agree in origin.
        assert_eq!(rows[0].ai_credits, 2);
        assert_eq!(rows[0].net_cost_micro_usd, MicroUsd(25_000));
    }

    #[test]
    fn parse_user_team_maps_user_to_team() {
        let ndjson = concat!(
            "{\"day\":\"2026-08-01\",\"user_id\":\"1001\",\"user_login\":\"octocat\",",
            "\"team_id\":\"9001\",\"slug\":\"eng-platform\"}\n",
        );
        let rows = parse_user_team(ndjson.as_bytes(), "user-teams-1-day", "2026-08-01").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].user_id, "1001");
        assert_eq!(rows[0].team_id, "9001");
        assert_eq!(rows[0].team_slug, "eng-platform");
    }

    #[test]
    fn malformed_line_surfaces_a_parse_error() {
        let err = parse_user_team(b"not-json\n", "user-teams-1-day", "2026-08-01").unwrap_err();
        match err {
            CopilotError::Parse { report, day, .. } => {
                assert_eq!(report, "user-teams-1-day");
                assert_eq!(day, "2026-08-01");
            }
            _ => panic!("expected a Parse error"),
        }
    }
}
