//! The once-per-run seat snapshot: `sync_seats` fetches the org's CURRENT
//! Copilot seat assignments and archives + parses + upserts them.

use cratestack::sqlx::PgPool;

use super::{ReportOutcome, parse_report_rows, seats_archive_key};
use crate::{auth::AppAuth, client::GithubClient, error::Result, replay::replay_report};

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
///
/// `freeze_writes` is the ADR-0014 cutover switch, as in [`super::sync_day`]:
/// when `true` the Postgres upsert + manifest write is skipped and the caller
/// emits the archived seat bytes as OTLP records through the usage ingest
/// sink; the parsed count is still returned.
#[expect(
    clippy::too_many_arguments,
    reason = "sync_seats threads the client, pool, tenant, org, snapshot_day, archive and freeze \
              flag by construction; grouping them would hide the boundaries it maps onto"
)]
pub async fn sync_seats(
    client: &GithubClient,
    pool: &PgPool,
    auth: &AppAuth<'_>,
    tenant_id: &str,
    org: &str,
    snapshot_day: &str,
    archive: impl AsyncFn(&str, &[u8]) -> Result<()>,
    freeze_writes: bool,
) -> Result<ReportOutcome> {
    let token = auth.token_for_org(org).await?;
    let fetched = client.fetch_seats(org, &token).await?;

    // Archive raw BEFORE parsing (RFC-0001: replay, not refetch), same
    // ordering as `ingest_one` above.
    let key = seats_archive_key(org, snapshot_day);
    let raw = fetched.to_archive_bytes();
    archive(&key, &raw).await?;

    let n = if freeze_writes {
        parse_report_rows(crate::SEATS_REPORT_TYPE, &raw, snapshot_day)?.len()
    } else {
        replay_report(
            pool,
            tenant_id,
            org,
            snapshot_day,
            crate::SEATS_REPORT_TYPE,
            &raw,
        )
        .await?
    };
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
