//! Backfill orchestration: compute the window, ingest each day, snapshot seats
//! once per run, and decide whether the run should exit non-zero.
//!
//! Split out of `sync.rs` (#178). The window math (`backfill_window`) is pure
//! and unit-tested; the `run_backfill_at`/`run_status` integration tests live
//! here and in `operators.rs`, sharing helpers from `test_util`.

use anyhow::Result;
use governance_copilot::{
    AppAuth, CopilotError, GithubClient, high_water_mark, sync_day, sync_seats,
};
use tracing::{info, warn};

use super::config::{COPILOT_DATA_LAG_DAYS, Config};
use crate::emit::Sink;

/// Raw bytes captured from the archive closure, keyed by report, so they can
/// be re-parsed and emitted as OTLP records after the fetch/archive/upsert.
type CapturedBytes = std::sync::Arc<std::sync::Mutex<Vec<(String, Vec<u8>)>>>;

/// Ingest a single day, archiving raw NDJSON through the configured sink.
///
/// When `sink` is present (an OTLP endpoint is configured), the raw bytes
/// archived for each report are also parsed and emitted as OTLP log records
/// (ADR-0014). The direct-Postgres write path inside `sync_day` stays active
/// until the cutover (lightbridge-authz#588); this is the replacement sink,
/// ready to be switched over.
pub(super) async fn ingest_day(
    client: &GithubClient,
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
    day: &str,
    sink: Option<&Sink>,
) -> Result<Vec<governance_copilot::ReportOutcome>> {
    let auth = AppAuth::new(cfg.app_id.clone(), cfg.private_key.clone(), client);
    let archive = cfg.archive.clone();
    // Capture the raw bytes each report archives so they can be re-parsed and
    // emitted as OTLP records after the fetch/archive/upsert completes. The
    // archive closure is the one place the live bytes are visible to the CLI.
    let captured: CapturedBytes = std::sync::Arc::default();
    let cap = std::sync::Arc::clone(&captured);
    let archive_fn = async |key: &str, bytes: &[u8]| {
        let report = key
            .rsplit('/')
            .next()
            .and_then(|f| {
                f.strip_suffix(".ndjson")
                    .or_else(|| f.strip_suffix(".json"))
            })
            .unwrap_or(key)
            .to_owned();
        cap.lock()
            .map_err(|_| CopilotError::Archive("capture lock poisoned".to_owned()))?
            .push((report, bytes.to_vec()));
        archive
            .write(key, bytes)
            .await
            .map_err(|e| CopilotError::Archive(format!("{e:#}")))
    };

    let outcomes = sync_day(
        client,
        pool,
        &auth,
        &cfg.tenant_id,
        &cfg.org,
        day,
        archive_fn,
    )
    .await?;

    if let Some(sink) = sink {
        // Clone out of the lock so the guard is dropped before any await.
        let captured = captured
            .lock()
            .map_err(|_| anyhow::anyhow!("capture lock poisoned while emitting OTLP records"))?
            .clone();
        for (report, bytes) in &captured {
            let rows = governance_copilot::parse_report_rows(report, bytes, day)?;
            sink.emit_rows(&cfg.tenant_id, &cfg.org, &rows).await?;
        }
    }

    for o in &outcomes {
        let host = o.host.clone().unwrap_or_else(|| "-".to_owned());
        info!(
            report = o.report,
            day = o.day,
            status = o.status,
            count = o.record_count,
            host = host,
            "ingested report"
        );
    }
    Ok(outcomes)
}

/// Snapshot the org's current Copilot seats, archiving the raw pages
/// through the configured sink before parsing (RFC-0001). Called exactly
/// ONCE per `sync` run by `run_backfill_at` below -- never per backfilled
/// day, never from `run_sync_day` -- see `governance_copilot::sync_seats`'s
/// doc comment for why looping this would fabricate a seat history that was
/// never actually observed.
async fn ingest_seats(
    client: &GithubClient,
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
    snapshot_day: &str,
    sink: Option<&Sink>,
) -> Result<governance_copilot::ReportOutcome> {
    let auth = AppAuth::new(cfg.app_id.clone(), cfg.private_key.clone(), client);
    let archive = cfg.archive.clone();
    let captured: CapturedBytes = std::sync::Arc::default();
    let cap = std::sync::Arc::clone(&captured);
    let archive_fn = async |key: &str, bytes: &[u8]| {
        let report = key
            .rsplit('/')
            .next()
            .and_then(|f| {
                f.strip_suffix(".ndjson")
                    .or_else(|| f.strip_suffix(".json"))
            })
            .unwrap_or(key)
            .to_owned();
        cap.lock()
            .map_err(|_| CopilotError::Archive("capture lock poisoned".to_owned()))?
            .push((report, bytes.to_vec()));
        archive
            .write(key, bytes)
            .await
            .map_err(|e| CopilotError::Archive(format!("{e:#}")))
    };

    let outcome = sync_seats(
        client,
        pool,
        &auth,
        &cfg.tenant_id,
        &cfg.org,
        snapshot_day,
        archive_fn,
    )
    .await?;

    if let Some(sink) = sink {
        // Clone out of the lock so the guard is dropped before any await.
        let captured = captured
            .lock()
            .map_err(|_| anyhow::anyhow!("capture lock poisoned while emitting OTLP records"))?
            .clone();
        for (report, bytes) in &captured {
            let rows = governance_copilot::parse_report_rows(report, bytes, snapshot_day)?;
            sink.emit_rows(&cfg.tenant_id, &cfg.org, &rows).await?;
        }
    }

    info!(
        report = outcome.report,
        day = outcome.day,
        status = outcome.status,
        count = outcome.record_count,
        "ingested seat snapshot"
    );
    Ok(outcome)
}

/// The `[start, end]` (inclusive) window of report days a `sync` run should
/// (re-)ingest. Three RFC-0001 requirements, one expression:
///
/// - **The upper bound is D-1, never D-0 ("today").** GitHub's Copilot
///   metrics have same-day latency: a request for `today` always fails with
///   HTTP 400 ("Date must be within the last year and not in the future").
///   RFC-0001 §Scheduling is explicit that a run "re-fetches D-1, D-2 and
///   D-3", not D-0 through D-2 -- confirmed live in production, where every
///   6h run was burning four guaranteed-to-fail requests (one per report
///   type) on `today` and logging a WARN for each, forever. `today` is still
///   the parameter's meaning (the real calendar day the run executes on);
///   every bound below is computed relative to it, but the window itself
///   never extends past `today - 1`.
/// - **Always re-fetch the trailing `lookback_days`**, regardless of where
///   the high-water mark sits. This is what makes a late-published report
///   self-heal with no operator action, and it is what stops the
///   high-water mark from permanently orphaning a day: a manifest row for a
///   *later* day (even an "empty" 204 one) can legitimately push the
///   high-water mark past an earlier day that never actually finished, but
///   that earlier day is still re-fetched here as long as it is within the
///   trailing window.
/// - **Still close any gap after the high-water mark** -- cold start, or
///   catching up after an outage that left it stale for a while.
/// - **Never walk back further than `max_backfill_days`**, so a cold start
///   (or a watermark that never advanced) cannot stampede the API.
///
/// `end = today - COPILOT_DATA_LAG_DAYS` (currently 1 day),
/// `start = max(min(hwm + 1, end, today - lookback_days), today - max_backfill_days)`.
/// See the `backfill_window_*` tests below for the three cases this is
/// required to get right (recent/stale/absent high-water mark) plus the
/// exact re-orphaning scenario from the review and the D-1-upper-bound
/// regression.
pub fn backfill_window(
    hwm: Option<chrono::NaiveDate>,
    today: chrono::NaiveDate,
    lookback_days: i64,
    max_backfill_days: i64,
) -> (chrono::NaiveDate, chrono::NaiveDate) {
    // The most recent day GitHub can ever answer for. This is the ONLY place
    // that decides that -- every caller (`run_backfill`/`run_backfill_at`)
    // passes real calendar `today` straight through; `sync-day` bypasses
    // this function entirely and is unaffected (an explicit operator
    // request for "today" should still be tried and get GitHub's real
    // error, not be silently blocked here).
    let end = today - chrono::Days::new(COPILOT_DATA_LAG_DAYS);
    let lookback_bound = today - chrono::Days::new(lookback_days.max(0) as u64);
    let max_backfill_bound = today - chrono::Days::new(max_backfill_days.max(0) as u64);
    let start = match hwm {
        Some(h) => {
            // Resume the day after the high-water mark, but never past the
            // last queryable day -- a stale-in-the-other-direction hwm (or a
            // 0-day lookback in a test config) must not push `start` beyond
            // `end`.
            let resume = (h + chrono::Days::new(1)).min(end);
            resume.min(lookback_bound).max(max_backfill_bound)
        }
        // No manifest row exists at all: treat as a cold start, bounded by
        // max_backfill_days rather than only the (much shorter) trailing
        // lookback window.
        None => max_backfill_bound,
    };
    (start, end)
}

/// The outcome of one `sync` (backfill) run.
#[derive(Debug, Clone)]
pub struct BackfillOutcome {
    /// Per-report outcomes for every day that ingested without error, plus
    /// the seat snapshot's own outcome when it succeeded (report type
    /// `SEATS_REPORT_TYPE`) -- so it shows up in the same
    /// `governance.copilot.reports`/`rows` push metrics as the day-based
    /// reports, without a second metric family.
    pub outcomes: Vec<governance_copilot::ReportOutcome>,
    /// Days in the window that ingested without error. An "empty" (204)
    /// report still counts as covered -- only a transport/auth/parse/storage
    /// error for the day counts as failed. Never incremented for the seat
    /// snapshot -- see `seats` below.
    pub covered: usize,
    /// Days in the window that errored: the day plus the error's rendered
    /// message (not the `anyhow::Error` itself -- `main` only needs to log
    /// and count these, and error types here are not `Clone`).
    pub failed: Vec<(String, String)>,
    /// Total days the computed window covered. Lets the caller distinguish
    /// "nothing to do" (window empty; fine) from "everything in a non-empty
    /// window failed" (must exit non-zero -- see `main`'s `Command::Sync`).
    pub window_days: usize,
    /// Outcome of the once-per-run seat snapshot (RFC-0001's headline use
    /// case: "who has a seat and has never used it"). `Ok(n)` = `n` seat
    /// rows upserted (`0` is a real "org has no seats" answer, not a
    /// failure). Tracked independently of `covered`/`failed`/`window_days`
    /// on purpose: seats and the day-based reports are different failure
    /// domains on a different axis entirely (once-per-run vs.
    /// once-per-day), so one failing must neither mask nor be masked by the
    /// other -- see `exit_result` below, which fails the run on either.
    pub seats: Result<usize, String>,
}

impl BackfillOutcome {
    /// Whether a `Command::Sync` run should exit non-zero: either the
    /// window was non-empty and every day in it failed (BLOCKER 1 from the
    /// pre-go-live review -- a totally broken run, dead credential, GitHub
    /// unreachable), or the once-per-run seat snapshot itself failed. Both
    /// are checked independently and either alone is sufficient to fail the
    /// run -- a healthy seat snapshot must not paper over every report
    /// failing, and healthy reports must not paper over a broken seat
    /// snapshot (RFC-0001's headline use case going silently unfilled is
    /// exactly the kind of failure `exit_result` exists to surface, not
    /// mask).
    ///
    /// Returns `Ok(())` only when both are healthy: nothing to do in an
    /// empty window (or a partial day failure, logged loudly by
    /// `run_backfill_at` already and re-attempted by the next run's
    /// trailing window -- BLOCKER 2) AND the seat snapshot succeeded.
    pub fn exit_result(&self) -> Result<()> {
        let reports_failed = self.window_days > 0 && self.covered == 0;
        let seats_failed = self.seats.is_err();
        match (reports_failed, seats_failed) {
            (true, true) => anyhow::bail!(
                "backfill covered 0 of {} day(s) in the window AND the seat snapshot failed \
                 (first report failure: {:?}; seats failure: {:?})",
                self.window_days,
                self.failed.first(),
                self.seats.as_ref().err()
            ),
            (true, false) => anyhow::bail!(
                "backfill covered 0 of {} day(s) in the window; every day failed \
                 (first failure: {:?})",
                self.window_days,
                self.failed.first()
            ),
            (false, true) => anyhow::bail!(
                "the once-per-run Copilot seat snapshot failed: {:?}",
                self.seats.as_ref().err()
            ),
            (false, false) => Ok(()),
        }
    }
}

/// Backfill: ingest the trailing lookback window plus any gap after the
/// high-water mark (see `backfill_window`), newest first so a late report
/// lands quickly. Does not decide the process exit code -- see `main`'s
/// `Command::Sync`, which uses `window_days`/`covered`/`failed` to do that.
pub async fn run_backfill(
    client: &GithubClient,
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
    sink: Option<&Sink>,
) -> Result<BackfillOutcome> {
    let today = chrono::Utc::now().date_naive();
    run_backfill_at(client, pool, cfg, today, sink).await
}

/// As `run_backfill`, but with `today` injected so tests can fix "now"
/// instead of racing the real clock.
pub async fn run_backfill_at(
    client: &GithubClient,
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
    today: chrono::NaiveDate,
    sink: Option<&Sink>,
) -> Result<BackfillOutcome> {
    let hwm = high_water_mark(pool, &cfg.tenant_id, "github_copilot").await?;
    let (start, end) = backfill_window(hwm, today, cfg.lookback_days, cfg.max_backfill_days);

    let mut days: Vec<chrono::NaiveDate> = Vec::new();
    let mut d = start;
    while d <= end {
        days.push(d);
        d = d + chrono::Days::new(1);
    }
    days.reverse(); // newest first so a late report lands quickly
    let window_days = days.len();

    info!(start = %start, end = %end, n = window_days, "backfill window");
    let mut all = Vec::new();
    let mut covered = 0usize;
    let mut failed = Vec::new();
    for day in days {
        let ds = day.format("%Y-%m-%d").to_string();
        match ingest_day(client, pool, cfg, &ds, sink).await {
            Ok(outcomes) => {
                covered += 1;
                all.extend(outcomes);
            }
            Err(e) => {
                warn!(day = ds, error = %e, "day failed; continuing backfill");
                failed.push((ds, format!("{e:#}")));
            }
        }
    }

    // Seats: exactly once per run, stamped with `today` -- deliberately
    // OUTSIDE the per-day loop above. GitHub's `/copilot/billing/seats` has
    // no `day` parameter at all, so calling this once per backfilled day
    // would write the SAME current snapshot under several different
    // `snapshot_day`s, fabricating a seat history that was never actually
    // observed on those days (see `governance_copilot::sync_seats`'s doc
    // comment). There is no backfill for seats, ever: a run that fails to
    // snapshot today's seats has lost that day's seat data permanently, not
    // deferred it to a later run.
    let today_str = today.format("%Y-%m-%d").to_string();
    let seats = match ingest_seats(client, pool, cfg, &today_str, sink).await {
        Ok(outcome) => {
            let n = outcome.record_count;
            all.push(outcome);
            Ok(n)
        }
        Err(e) => {
            warn!(error = %e, "seat snapshot failed");
            Err(format!("{e:#}"))
        }
    };

    Ok(BackfillOutcome {
        outcomes: all,
        covered,
        failed,
        window_days,
        seats,
    })
}

#[cfg(test)]
mod tests;
