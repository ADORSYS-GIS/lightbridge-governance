//! The collector CLI. Runs as the `copilot-sync` CronJob and as an operator tool.
//!
//! There is deliberately NO separate backfill Job: a one-shot k8s Job is immutable
//! and re-running it means deleting the object out-of-band, which ArgoCD selfHeal
//! fights. `sync` reads the high-water mark from the DB and backfills up to 28 days
//! when it is behind -- which also gives late-report recovery for free (ADR-0006).

use anyhow::Result;
use clap::{Parser, Subcommand};

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
    /// Ingest the most recent days, backfilling to the high-water mark if behind.
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

    // TODO(RFC-0001): dispatch once the connector lands.
    let _ = args.database_url;
    Ok(())
}
