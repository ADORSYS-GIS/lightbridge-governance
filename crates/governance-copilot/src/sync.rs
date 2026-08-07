//! Orchestrates a single day's ingestion: authenticate -> fetch each report ->
//! archive raw to S3 -> parse -> upsert -> record an ingest manifest.
//!
//! The S3 archive is injected (`archive`) so this module is unit-testable
//! without object storage and step 4 (the real S3 writer) plugs in behind the
//! same signature. The raw bytes are archived BEFORE parsing, per RFC-0001: a
//! parsing bug is replayed from the archive, never refetched.

use cratestack::sqlx::PgPool;

use crate::{
    auth::AppAuth,
    client::GithubClient,
    error::{CopilotError, Result},
    parse::{parse_org_daily, parse_repo_daily, parse_seats, parse_user_daily, parse_user_team},
    store::{
        upsert_manifest, upsert_org_daily, upsert_repo_daily, upsert_seat_snapshot,
        upsert_user_daily, upsert_user_team,
    },
};

/// Key under which a report's raw NDJSON is archived, relative to the sink's
/// own prefix (`copilot-governance/raw/` on S3, `RAW_DIR` locally; RFC-0001).
/// Must stay in lockstep with `Archive::list_day`/`read` — replay depends on
/// it. Printable only; never a URL.
pub fn archive_key(org: &str, report: &str, day: &str) -> String {
    format!("org={org}/day={day}/{report}.ndjson")
}

/// Key under which the archived seat listing lives, relative to the sink's
/// own prefix. Deterministic per (org, day): a same-day re-run overwrites
/// this one file regardless of how many pages the current seat count spans
/// (`FetchedSeats::to_archive_bytes` always produces exactly one JSON
/// document). `.json`, not `.ndjson`, on purpose: this is a single JSON
/// array document, not one row of NDJSON per line like every `archive_key`
/// above.
pub fn seats_archive_key(org: &str, snapshot_day: &str) -> String {
    format!(
        "org={org}/day={snapshot_day}/{}.json",
        crate::SEATS_REPORT_TYPE
    )
}

/// Outcome of ingesting one report for one day.
#[derive(Debug, Clone)]
pub struct ReportOutcome {
    pub report: String,
    pub day: String,
    pub status: String,
    pub record_count: usize,
    pub host: Option<String>,
}

/// Ingest a single day across all org-scope reports.
pub async fn sync_day(
    client: &GithubClient,
    pool: &PgPool,
    auth: &AppAuth<'_>,
    tenant_id: &str,
    org: &str,
    day: &str,
    archive: impl AsyncFn(&str, &[u8]) -> Result<()>,
) -> Result<Vec<ReportOutcome>> {
    let token = auth.token_for_org(org).await?;
    let mut outcomes = Vec::new();

    for report in crate::REPORTS {
        let outcome =
            ingest_one(client, pool, tenant_id, org, day, report, &token, &archive).await?;
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

#[expect(
    clippy::too_many_arguments,
    reason = "ingest_one threads the client, pool, tenant, org, day, report, token and archive by \
              construction; grouping them would hide the boundaries sync_day maps onto"
)]
async fn ingest_one(
    client: &GithubClient,
    pool: &PgPool,
    tenant_id: &str,
    org: &str,
    day: &str,
    report: &str,
    token: &crate::RawSecret,
    archive: &impl AsyncFn(&str, &[u8]) -> Result<()>,
) -> Result<ReportOutcome> {
    let downloaded = client.fetch_report(org, report, day, token).await?;
    let host = downloaded.host.clone();

    // No data for the day (204/empty): record a manifest and move on. This is
    // NOT a failure -- the report simply has no rows yet.
    if downloaded.empty {
        upsert_manifest(
            pool,
            tenant_id,
            "github_copilot",
            org,
            report,
            day,
            "empty",
            0,
        )
        .await?;
        return Ok(ReportOutcome {
            report: report.to_owned(),
            day: day.to_owned(),
            status: "empty".to_owned(),
            record_count: 0,
            host,
        });
    }

    // Archive raw BEFORE parsing (RFC-0001: replay, not refetch).
    let key = archive_key(org, report, day);
    archive(&key, &downloaded.bytes).await?;

    let outcome = replay_report(pool, tenant_id, org, day, report, &downloaded.bytes).await?;
    Ok(ReportOutcome {
        report: report.to_owned(),
        day: day.to_owned(),
        status: "ok".to_owned(),
        record_count: outcome,
        host,
    })
}

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
        // The seat snapshot shares this code path too (see `sync_seats`
        // below), so `governance-ctl replay` can recover from a parsing bug
        // in `parse_seats` the same way it does for the four day-based
        // reports -- the archived `bytes` here are exactly what
        // `FetchedSeats::to_archive_bytes` wrote, whether this call came
        // from the live fetch path or a replay of that archive. `day` is
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

/// Snapshot the org's CURRENT Copilot seat assignments (RFC-0001's headline
/// use case: "who has a seat and has never used it").
///
/// Deliberately NOT part of `REPORTS`/`sync_day`'s per-day loop, and takes
/// no historical `day` to fetch by -- GitHub's `/copilot/billing/seats` has
/// no `day` parameter at all and always returns "right now". Looping this
/// once per backfilled day would write the SAME current snapshot under
/// several different `snapshot_day`s, fabricating a seat history that was
/// never actually observed on those days. There is no backfill for seats,
/// ever: a day the run failed to snapshot is gone, not recoverable by a
/// later run re-fetching (though it IS recoverable by replaying this run's
/// own archive, via `replay_report`'s `SEATS_REPORT_TYPE` arm above, if a
/// *parsing* bug is what needs fixing).
///
/// Callers call this exactly ONCE per run, with `snapshot_day` = today; see
/// `app/governance-ctl/src/sync.rs::run_backfill_at`, which calls this
/// outside its per-day loop, never inside it.
pub async fn sync_seats(
    client: &GithubClient,
    pool: &PgPool,
    auth: &AppAuth<'_>,
    tenant_id: &str,
    org: &str,
    snapshot_day: &str,
    archive: impl AsyncFn(&str, &[u8]) -> Result<()>,
) -> Result<ReportOutcome> {
    let token = auth.token_for_org(org).await?;
    let fetched = client.fetch_seats(org, &token).await?;

    // Archive raw BEFORE parsing (RFC-0001: replay, not refetch), same
    // ordering as `ingest_one` above.
    let key = seats_archive_key(org, snapshot_day);
    let raw = fetched.to_archive_bytes();
    archive(&key, &raw).await?;

    let n = replay_report(
        pool,
        tenant_id,
        org,
        snapshot_day,
        crate::SEATS_REPORT_TYPE,
        &raw,
    )
    .await?;
    Ok(ReportOutcome {
        report: crate::SEATS_REPORT_TYPE.to_owned(),
        day: snapshot_day.to_owned(),
        // Unlike the four reports' `empty` (GitHub's HTTP 204 = "not
        // published yet"), GitHub always returns 200 for the seats
        // endpoint, including for a zero-seat org -- `seats: []` is a
        // successfully queried "no seats" answer, not missing data, so
        // there is no seats-specific "empty" status to distinguish here.
        status: "ok".to_owned(),
        record_count: n,
        host: None,
    })
}
