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

    /// Reproduces the production failure directly (2026-08-07): live GitHub
    /// `repos-1-day` NDJSON sends `repo_id` as a bare JSON integer, not the
    /// string every fixture here (and the vendor docs) assumed. Every day's
    /// repo report failed to parse with "invalid type: integer, expected a
    /// string" until this was fixed.
    #[test]
    fn parse_repo_daily_accepts_an_integer_repo_id() {
        let ndjson = concat!(
            "{\"day\":\"2026-08-01\",\"repo_id\":844522530,\"repo_name\":\"lightbridge-governance\",",
            "\"coding_agent_activity\":3,\"code_review_activity\":1,\"pull_request_activity\":2}\n",
        );
        let rows = parse_repo_daily(ndjson.as_bytes(), "repos-1-day", "2026-08-01").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].repository_id, "844522530");
        assert_eq!(rows[0].coding_agent_activity, 3);
    }

    /// The other three report types type their id fields the identical way
    /// (`Option<String>`) on the same wrong assumption -- only `repos-1-day`
    /// had non-empty rows the day this was caught, so these three were
    /// latent, not actually exercised yet. Locking in the fix for all of
    /// them, not just the one that happened to fail first.
    #[test]
    fn parse_org_daily_accepts_an_integer_organization_id() {
        let ndjson = concat!(
            "{\"day\":\"2026-08-01\",\"organization_id\":139577169,",
            "\"total_active_users\":10,\"total_engaged_users\":4,",
            "\"total_completions\":120,\"total_chat_engagements\":30}\n",
        );
        let rows = parse_org_daily(ndjson.as_bytes(), "organization-1-day", "2026-08-01").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].organization_id, "139577169");
    }

    #[test]
    fn parse_user_daily_accepts_an_integer_user_id() {
        let ndjson = concat!(
            "{\"day\":\"2026-08-01\",\"user_id\":1001,\"user_login\":\"octocat\",",
            "\"total_engagements\":42,\"total_completions\":20,\"ai_credits\":2.5}\n",
        );
        let rows = parse_user_daily(ndjson.as_bytes(), "users-1-day", "2026-08-01").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider_user_id, "1001");
    }

    #[test]
    fn parse_user_team_accepts_integer_user_and_team_ids() {
        let ndjson = concat!(
            "{\"day\":\"2026-08-01\",\"user_id\":1001,\"user_login\":\"octocat\",",
            "\"team_id\":9001,\"slug\":\"eng-platform\"}\n",
        );
        let rows = parse_user_team(ndjson.as_bytes(), "user-teams-1-day", "2026-08-01").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].user_id, "1001");
        assert_eq!(rows[0].team_id, "9001");
    }

    #[test]
    fn seat_state_is_active_without_a_pending_cancellation_date() {
        assert_eq!(seat_state(None), "active");
    }

    #[test]
    fn seat_state_is_pending_cancellation_when_the_date_is_present() {
        assert_eq!(seat_state(Some("2026-09-01")), "pending_cancellation");
    }

    #[test]
    fn parse_seat_timestamp_parses_a_valid_rfc3339_value() {
        let parsed = parse_seat_timestamp(Some("2026-08-01T12:00:00Z")).unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-08-01T12:00:00+00:00");
    }

    /// An absent or malformed timestamp must become `None` (unknown), never
    /// a fabricated default -- this is what the whole test proves, not just
    /// that a good value parses.
    #[test]
    fn parse_seat_timestamp_treats_absence_and_garbage_as_unknown() {
        assert_eq!(parse_seat_timestamp(None), None);
        assert_eq!(parse_seat_timestamp(Some("not-a-timestamp")), None);
    }

    fn seats_archive(pages: &[&str]) -> Vec<u8> {
        let joined = pages.join(",");
        format!("[{joined}]").into_bytes()
    }

    #[test]
    fn parse_seats_maps_github_fields_onto_the_normalized_row() {
        let page = concat!(
            r#"{"total_seats":1,"seats":[{"#,
            r#""created_at":"2026-01-01T00:00:00Z","#,
            r#""last_activity_at":"2026-08-01T09:30:00Z","#,
            r#""last_activity_editor":"vscode/1.90.0/copilot/1.200.0","#,
            r#""pending_cancellation_date":null,"#,
            r#""assignee":{"id":1001,"login":"octocat"}"#,
            r#"}]}"#,
        );
        let rows = parse_seats(&seats_archive(&[page]), "billing-seats", "2026-08-07").unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.provider_user_id, "1001");
        assert_eq!(row.user_login, "octocat");
        assert_eq!(row.snapshot_day, "2026-08-07");
        assert_eq!(row.seat_state, "active");
        assert_eq!(
            row.last_activity_editor.as_deref(),
            Some("vscode/1.90.0/copilot/1.200.0")
        );
        assert!(row.seat_assigned_at.is_some());
        assert!(row.last_activity_at.is_some());
    }

    /// A seat whose `last_activity_at` is entirely absent (never used) must
    /// stay `None`, not become a fabricated "never" sentinel that would
    /// read as a real timestamp downstream -- this is RFC-0001's exact
    /// motivating question ("who has a seat and has never used it").
    #[test]
    fn parse_seats_treats_a_never_used_seat_as_null_not_a_default() {
        let page = concat!(
            r#"{"seats":[{"#,
            r#""created_at":"2026-01-01T00:00:00Z","#,
            r#""last_activity_at":null,"#,
            r#""last_activity_editor":null,"#,
            r#""pending_cancellation_date":null,"#,
            r#""assignee":{"id":2002,"login":"neveruser"}"#,
            r#"}]}"#,
        );
        let rows = parse_seats(&seats_archive(&[page]), "billing-seats", "2026-08-07").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].last_activity_at, None);
        assert_eq!(rows[0].last_activity_editor, None);
    }

    #[test]
    fn parse_seats_marks_pending_cancellation_from_the_date_field() {
        let page = concat!(
            r#"{"seats":[{"#,
            r#""created_at":"2026-01-01T00:00:00Z","#,
            r#""pending_cancellation_date":"2026-09-01","#,
            r#""assignee":{"id":3003,"login":"leaving"}"#,
            r#"}]}"#,
        );
        let rows = parse_seats(&seats_archive(&[page]), "billing-seats", "2026-08-07").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seat_state, "pending_cancellation");
    }

    /// A row with no assignee at all is skipped, not fabricated -- mirrors
    /// `parse_org_daily`'s "no org id" handling.
    #[test]
    fn parse_seats_skips_a_row_with_no_assignee() {
        let page = r#"{"seats":[{"created_at":"2026-01-01T00:00:00Z"}]}"#;
        let rows = parse_seats(&seats_archive(&[page]), "billing-seats", "2026-08-07").unwrap();
        assert!(rows.is_empty());
    }

    /// An empty org's page (`seats: []`) must parse to zero rows, not an
    /// error -- this is the "empty org" case the manifest's "empty" status
    /// depends on.
    #[test]
    fn parse_seats_on_an_empty_org_yields_zero_rows() {
        let page = r#"{"total_seats":0,"seats":[]}"#;
        let rows = parse_seats(&seats_archive(&[page]), "billing-seats", "2026-08-07").unwrap();
        assert!(rows.is_empty());
    }

    /// Multiple archived pages must all contribute rows -- proves
    /// `parse_seats` walks every page in the archived JSON array, not just
    /// the first.
    #[test]
    fn parse_seats_combines_rows_from_every_page() {
        let page1 = r#"{"seats":[{"assignee":{"id":1,"login":"a"}}]}"#;
        let page2 = r#"{"seats":[{"assignee":{"id":2,"login":"b"}}]}"#;
        let rows = parse_seats(
            &seats_archive(&[page1, page2]),
            "billing-seats",
            "2026-08-07",
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        let ids: Vec<&str> = rows.iter().map(|r| r.provider_user_id.as_str()).collect();
        assert_eq!(ids, vec!["1", "2"]);
    }

    /// Live GitHub sends `assignee.id` as a bare JSON integer -- the exact
    /// production failure `flexible_id` already fixed for the other four
    /// reports (see the `repos-1-day` regression test above). Applied here
    /// too, on the same assumption.
    #[test]
    fn parse_seats_accepts_an_integer_assignee_id() {
        let page = r#"{"seats":[{"assignee":{"id":844522530,"login":"octocat"}}]}"#;
        let rows = parse_seats(&seats_archive(&[page]), "billing-seats", "2026-08-07").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider_user_id, "844522530");
    }

    #[test]
    fn parse_seats_on_a_malformed_archive_surfaces_a_parse_error() {
        let err = parse_seats(b"not-json", "billing-seats", "2026-08-07").unwrap_err();
        match err {
            CopilotError::Parse { report, day, .. } => {
                assert_eq!(report, "billing-seats");
                assert_eq!(day, "2026-08-07");
            }
            _ => panic!("expected a Parse error"),
        }
    }
}
