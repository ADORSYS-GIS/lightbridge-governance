//! The per-day report ingestion loop: `sync_day` fetches every org-scope
//! report for one day and `ingest_one` archives + parses + upserts each.

use cratestack::sqlx::PgPool;

use super::{ReportOutcome, archive_key, parse_report_rows};
use crate::{
    RawSecret, auth::AppAuth, client::GithubClient, error::Result, replay::replay_report,
    store::upsert_manifest,
};

/// Ingest a single day across all org-scope reports.
///
/// `freeze_writes` is the ADR-0014 cutover switch: when `true`, the
/// direct-Postgres write path (`replay_report`'s upsert + manifest) is
/// skipped and the raw bytes are only archived -- the caller is expected to
/// emit them as OTLP log records through the usage ingest sink. The parsed
/// row count is still returned so the caller can report it and assert counts.
/// When `false` (the pre-cutover default), the legacy Postgres write happens
/// as before.
#[expect(
    clippy::too_many_arguments,
    reason = "sync_day threads the client, pool, tenant, org, day, archive and freeze flag by \
              construction; grouping them would hide the boundaries it maps onto"
)]
pub async fn sync_day(
    client: &GithubClient,
    pool: &PgPool,
    auth: &AppAuth<'_>,
    tenant_id: &str,
    org: &str,
    day: &str,
    archive: impl AsyncFn(&str, &[u8]) -> Result<()>,
    freeze_writes: bool,
) -> Result<Vec<ReportOutcome>> {
    let token = auth.token_for_org(org).await?;
    let mut outcomes = Vec::new();

    for report in crate::REPORTS {
        let outcome = ingest_one(
            client,
            pool,
            tenant_id,
            org,
            day,
            report,
            &token,
            &archive,
            freeze_writes,
        )
        .await?;
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

#[expect(
    clippy::too_many_arguments,
    reason = "ingest_one threads the client, pool, tenant, org, day, report, token, archive and \
              freeze flag by construction; grouping them would hide the boundaries sync_day maps onto"
)]
async fn ingest_one(
    client: &GithubClient,
    pool: &PgPool,
    tenant_id: &str,
    org: &str,
    day: &str,
    report: &str,
    token: &RawSecret,
    archive: &impl AsyncFn(&str, &[u8]) -> Result<()>,
    freeze_writes: bool,
) -> Result<ReportOutcome> {
    let downloaded = client.fetch_report(org, report, day, token).await?;
    let host = downloaded.host.clone();

    // No data for the day (204/empty): record a manifest and move on. This is
    // NOT a failure -- the report simply has no rows yet.
    //
    // Under a freeze (ADR-0014 cutover) the manifest write is skipped, exactly
    // like the non-empty path below: the OTLP sink is the ONLY write path and
    // nothing touches the governance telemetry tables. Writing a manifest here
    // while freeze is on would advance the high-water mark for empty days while
    // non-empty days (which skip `replay_report` under freeze) leave none,
    // splitting the watermark and making `verify-counts`/`decommission` report
    // a false green against a half-written manifest table.
    if downloaded.empty {
        if !freeze_writes {
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
        }
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

    // When writes are frozen (ADR-0014 cutover) the Postgres upsert + manifest
    // write is skipped -- the caller emits the archived bytes as OTLP records
    // through the usage ingest sink. The parsed count is still returned so the
    // emit and the count-assertion harness have a number to compare.
    let outcome = if freeze_writes {
        parse_report_rows(report, &downloaded.bytes, day)?.len()
    } else {
        replay_report(pool, tenant_id, org, day, report, &downloaded.bytes).await?
    };
    Ok(ReportOutcome {
        report: report.to_owned(),
        day: day.to_owned(),
        status: "ok".to_owned(),
        record_count: outcome,
        host,
    })
}
