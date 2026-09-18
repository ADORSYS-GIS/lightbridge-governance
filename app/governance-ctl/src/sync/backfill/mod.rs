//! Backfill orchestration: compute the window, ingest each day, snapshot seats
//! once per run, and decide whether the run should exit non-zero.
//!
//! Split out of `sync.rs` (#178) and then further into focused submodules:
//! `ingest` (fetch/archive/upsert/emit), `window` (the pure window math), and
//! `outcome` (the run result + exit decision). This root holds the
//! orchestration (`run_backfill`/`run_backfill_at`) and re-exports the public
//! surface `main.rs`/`metrics.rs`/`operators.rs` consume.

mod ingest;
mod outcome;
mod window;

use anyhow::Result;
use governance_copilot::{GithubClient, high_water_mark};
pub(super) use ingest::ingest_day;
pub use outcome::BackfillOutcome;
use tracing::{info, warn};
pub use window::backfill_window;

use super::config::Config;
use crate::emit::Sink;

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
    let seats = match ingest::ingest_seats(client, pool, cfg, &today_str, sink).await {
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
mod outcome_tests;
#[cfg(test)]
mod seats_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod window_tests;
