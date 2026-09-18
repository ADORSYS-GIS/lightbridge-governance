//! The archive-facing and status operators: `sync-day`, `replay`, `verify`,
//! and `status`. Split out of `sync.rs` (#178). These are the subcommands an
//! operator runs directly, as opposed to the scheduled backfill in
//! `backfill.rs`. `replay` lives in its own submodule (it is the largest).

mod replay;

use anyhow::{Context, Result};
use governance_copilot::{high_water_mark, unmapped_user_count, verify_manifests};
pub use replay::run_replay;
use tracing::{info, warn};

use super::config::Config;
use crate::emit::Sink;

/// Ingest one explicit day. Idempotent.
pub async fn run_sync_day(
    client: &governance_copilot::GithubClient,
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
    day: &str,
    sink: Option<&Sink>,
) -> Result<Vec<governance_copilot::ReportOutcome>> {
    // Validate the day string early so a typo fails before any network call.
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .with_context(|| format!("invalid day {day:?}, want YYYY-MM-DD"))?;
    super::backfill::ingest_day(client, pool, cfg, day, sink).await
}

/// Reconcile stored row counts against the manifests and report drift.
/// Returns the number of mismatching manifest rows.
pub async fn run_verify(pool: &cratestack::sqlx::PgPool, cfg: &Config) -> Result<usize> {
    let drift = verify_manifests(pool, &cfg.tenant_id, "github_copilot", &cfg.org).await?;
    for d in &drift {
        warn!(
            day = d.day,
            report = d.report,
            status = d.status,
            expected = d.expected,
            actual = d.actual,
            "manifest/stored row-count drift"
        );
    }
    info!(mismatch = drift.len(), "verification complete");
    Ok(drift.len())
}

/// Whether a deployment has ever completed a Copilot sync, and if so how
/// stale the most recent success is.
///
/// This used to be a `(-1, -1)` sentinel pair of ints, and `push_status_
/// metrics` folded `-1` into `0` via `.max(0)` -- making "never synced"
/// metrically identical to "synced 0 days ago". An age-based alert
/// ("report older than 72h") could then never fire for a deployment that
/// had never run at all (BLOCKER 3). An enum makes "unknown" a case the
/// caller must handle, not a magic number that can be silently clamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    /// No `ingest_manifests` row exists for this tenant/provider at all.
    NeverSynced,
    /// At least one manifest row exists.
    Synced {
        /// `today - MAX(report_day)`, in days.
        age_days: i64,
        unmapped_users: i64,
    },
}

/// Connector status: last success, report age, unmapped users.
pub async fn run_status(pool: &cratestack::sqlx::PgPool, cfg: &Config) -> Result<SyncStatus> {
    let Some(last) = high_water_mark(pool, &cfg.tenant_id, "github_copilot").await? else {
        info!("no manifests yet; nothing has been ingested");
        return Ok(SyncStatus::NeverSynced);
    };
    let today = chrono::Utc::now().date_naive();
    let age_days = today.signed_duration_since(last).num_days();
    let unmapped_users =
        unmapped_user_count(pool, &cfg.tenant_id, &cfg.org, &last.to_string()).await?;
    info!(
        last = %last,
        age_days,
        unmapped_users,
        "connector status"
    );
    Ok(SyncStatus::Synced {
        age_days,
        unmapped_users,
    })
}
