//! The collector CLI. Runs as the `copilot-sync` CronJob and as an operator tool.
//!
//! There is deliberately NO separate backfill Job: a one-shot k8s Job is immutable
//! and re-running it means deleting the object out-of-band, which ArgoCD selfHeal
//! fights. `sync` always re-fetches a trailing lookback window (RFC-0001: D-1,
//! D-2, D-3) and separately fills any gap after the high-water mark, bounded so a
//! cold start cannot walk back forever (`sync::backfill_window`) -- which also
//! gives late-report recovery for free (ADR-0006).
//!
//! `Command::Sync` exits non-zero when the computed window was non-empty but
//! EVERY day in it failed (`covered == 0`) -- that is a totally broken run (dead
//! credential, GitHub unreachable), and the CronJob's `backoffLimit`/alerting
//! must engage rather than silently exiting 0 (pre-go-live review, BLOCKER 1). A
//! partial failure (some days ok, some not) still exits 0: it is logged loudly,
//! counted, and the failed days are re-attempted by the next run's trailing
//! window (BLOCKER 2) -- failing the whole job for a partial failure would only
//! be noise.

use anyhow::Result;
use clap::{Parser, Subcommand};

mod archive;
mod metrics;
mod sync;
#[cfg(test)]
mod test_support;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Operator and CronJob entry point.
#[derive(Debug, Parser)]
#[command(name = "governance-ctl", version, about)]
struct Args {
    /// Postgres connection string.
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,

    #[command(subcommand)]
    command: Command,
}

/// Subcommands. One image, several verbs.
#[derive(Debug, Subcommand)]
enum Command {
    /// Ingest the trailing lookback window, plus any gap after the
    /// high-water mark if behind (see `sync::backfill_window`).
    Sync,
    /// Ingest one specific report day (YYYY-MM-DD). Idempotent.
    SyncDay {
        /// The report day to fetch.
        day: String,
    },
    /// Re-derive normalized rows from the raw S3 archive without calling GitHub.
    Replay {
        /// First day of the range to replay.
        from: String,
        /// Last day of the range to replay.
        to: String,
    },
    /// Reconcile stored row counts against the manifests and report drift.
    Verify,
    /// Print connector status: last success, report age, unmapped users.
    Status,
    /// Apply the schema migrations cratestack derives from
    /// `schema/governance.cstack`. There are no hand-written migration files
    /// (ADR-0009).
    Migrate,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt().json().init();

    let args = Args::parse();
    tracing::info!(command = ?args.command, "governance-ctl invoked");

    match args.command {
        Command::Migrate => {
            let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
            let applied = governance_core::migrate::run(&pool).await?;
            if applied.is_empty() {
                tracing::info!("no pending migrations; already current");
            } else {
                tracing::info!(applied = ?applied, "migrations applied");
            }
        }
        Command::Sync => {
            let cfg = sync::Config::from_env().await?;
            let client = governance_copilot::GithubClient::for_github()?;
            let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
            let result = sync::run_backfill(&client, &pool, &cfg).await?;
            if let Some(endpoint) = metrics::endpoint_from_env() {
                metrics::push_run_metrics(
                    &endpoint,
                    "sync",
                    &result.outcomes,
                    result.covered as u64,
                )
                .await;
            }
            // BLOCKER 1: a non-empty window where every day failed must exit
            // non-zero so the CronJob's backoffLimit/alerting engage instead
            // of a silently-successful process. See the module doc comment
            // above and `BackfillOutcome::exit_result`'s own doc comment.
            result.exit_result()?;
        }
        Command::SyncDay { day } => {
            let cfg = sync::Config::from_env().await?;
            let client = governance_copilot::GithubClient::for_github()?;
            let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
            let outcomes = sync::run_sync_day(&client, &pool, &cfg, &day).await?;
            if let Some(endpoint) = metrics::endpoint_from_env() {
                metrics::push_run_metrics(&endpoint, "sync_day", &outcomes, 1).await;
            }
        }
        Command::Replay { from, to } => {
            let cfg = sync::Config::from_env().await?;
            let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
            sync::run_replay(&pool, &cfg, &from, &to).await?;
        }
        Command::Verify => {
            let cfg = sync::Config::from_env().await?;
            let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
            let mismatch = sync::run_verify(&pool, &cfg).await?;
            if let Some(endpoint) = metrics::endpoint_from_env() {
                metrics::push_verify_metrics(&endpoint, mismatch).await;
            }
        }
        Command::Status => {
            let cfg = sync::Config::from_env().await?;
            let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
            let status = sync::run_status(&pool, &cfg).await?;
            if let Some(endpoint) = metrics::endpoint_from_env() {
                metrics::push_status_metrics(&endpoint, status).await;
            }
        }
    }
    Ok(())
}
