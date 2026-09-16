//! Parse + upsert raw report bytes, shared by live ingestion and replay.

use cratestack::sqlx::PgPool;

use crate::{
    error::{CopilotError, Result},
    parse::{parse_org_daily, parse_repo_daily, parse_seats, parse_user_daily, parse_user_team},
    store::{
        upsert_manifest, upsert_org_daily, upsert_repo_daily, upsert_seat_snapshot,
        upsert_user_daily, upsert_user_team,
    },
};

/// Parse + upsert raw report NDJSON for a report type. Returns row count.
///
/// This is the single code path shared by live ingestion (`sync_day`) and
/// `replay` from the raw archive: both end in the same parse, upsert and
/// manifest write, so a replayed day is byte-identical to the original run.
pub async fn replay_report(
    pool: &PgPool,
    tenant_id: &str,
    org: &str,
    day: &str,
    report: &str,
    bytes: &[u8],
) -> Result<usize> {
    match report {
        "organization-1-day" => {
            let rows = parse_org_daily(bytes, report, day)?;
            let n = upsert_org_daily(pool, tenant_id, &rows).await?;
            upsert_manifest(pool, tenant_id, "github_copilot", org, report, day, "ok", n).await?;
            Ok(n)
        }
        "users-1-day" => {
            let rows = parse_user_daily(bytes, report, day)?;
            let n = upsert_user_daily(pool, tenant_id, org, &rows).await?;
            upsert_manifest(pool, tenant_id, "github_copilot", org, report, day, "ok", n).await?;
            Ok(n)
        }
        "repos-1-day" => {
            let rows = parse_repo_daily(bytes, report, day)?;
            let n = upsert_repo_daily(pool, tenant_id, org, &rows).await?;
            upsert_manifest(pool, tenant_id, "github_copilot", org, report, day, "ok", n).await?;
            Ok(n)
        }
        "user-teams-1-day" => {
            let rows = parse_user_team(bytes, report, day)?;
            let n = upsert_user_team(pool, tenant_id, org, &rows).await?;
            upsert_manifest(pool, tenant_id, "github_copilot", org, report, day, "ok", n).await?;
            Ok(n)
        }
        // The seat snapshot shares this code path too (see `sync_seats`), so
        // `governance-ctl replay` can recover from a parsing bug in
        // `parse_seats` the same way it does for the four day-based reports
        // -- the archived `bytes` here are exactly what
        // `FetchedSeats::to_archive_bytes` wrote, whether this call came from
        // the live fetch path or a replay of that archive. `day` is
        // `snapshot_day` for this report type -- see `SeatSnapshot`'s doc
        // comment for why that is never a historical day.
        crate::SEATS_REPORT_TYPE => {
            let rows = parse_seats(bytes, report, day)?;
            let n = upsert_seat_snapshot(pool, tenant_id, org, &rows).await?;
            upsert_manifest(pool, tenant_id, "github_copilot", org, report, day, "ok", n).await?;
            Ok(n)
        }
        other => Err(CopilotError::github(
            "sync",
            0,
            format!("unknown report type {other} in REPORTS"),
        )),
    }
}
