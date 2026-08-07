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
    /// Report per-provider identity attribution (attributed/unattributed/
    /// mismatched) and fail if any provider has unattributed executions.
    VerifyAttribution,
    /// Sync the identity directory (Keycloak) into `identity_maps`.
    IdentitySync {
        /// Tenant whose identity maps are synced (ADR-0001).
        #[arg(long)]
        tenant: String,
        /// Provider namespace, e.g. `github_copilot`.
        #[arg(long)]
        provider: String,
        /// Path to the directory file: one JSON array of `{provider_user_id,
        /// internal_user_id}` entries per line or a single array.
        #[arg(long)]
        file: String,
    },
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
        Command::VerifyAttribution => {
            let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
            let tenant = resolve_single_tenant(&pool).await?;
            let report = governance_core::identity::verify_attribution(&pool, &tenant).await?;
            for provider in &report.providers {
                tracing::info!(
                    provider = %provider.provider,
                    attributed = provider.attributed,
                    unattributed = provider.unattributed,
                    mismatched = provider.mismatched,
                    "verify-attribution: attribution"
                );
            }
            if report.has_unattributed() {
                anyhow::bail!("verify-attribution: unattributed executions present; attribution is incomplete");
            }
        }
        Command::IdentitySync {
            tenant,
            provider,
            file,
        } => {
            let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
            let entries = read_directory(&file)?;
            let report = governance_core::identity::sync_identity_directory(
                &pool, &tenant, &provider, &entries,
            )
            .await?;
            tracing::info!(
                inserted = report.inserted,
                repointed = report.repointed,
                unchanged = report.unchanged,
                "identity-sync: complete"
            );
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

/// The single tenant this deployment serves (ADR-0001). `verify` reads the
/// tenant from the `tenants` table rather than trusting a caller-supplied id.
async fn resolve_single_tenant(pool: &cratestack::sqlx::PgPool) -> Result<String> {
    let tenants: Vec<(String,)> = cratestack::sqlx::query_as("SELECT id FROM tenants ORDER BY id")
        .fetch_all(pool)
        .await?;
    match tenants.as_slice() {
        [(id,)] => Ok(id.clone()),
        [] => anyhow::bail!("verify: no tenant provisioned (ADR-0001 requires exactly one)"),
        _ => anyhow::bail!(
            "verify: {} tenants found; ADR-0001 is single-tenant per deployment",
            tenants.len()
        ),
    }
}

/// Parses the directory file: a JSON array of `{provider_user_id,
/// internal_user_id}` objects.
fn read_directory(file: &str) -> Result<Vec<governance_core::identity::DirectoryEntry>> {
    let raw = std::fs::read_to_string(file)?;
    let entries: Vec<DirectoryEntryJson> = serde_json::from_str(&raw)?;
    Ok(entries
        .into_iter()
        .map(|e| governance_core::identity::DirectoryEntry {
            provider_user_id: e.provider_user_id,
            internal_user_id: e.internal_user_id,
        })
        .collect())
}

#[derive(serde::Deserialize)]
struct DirectoryEntryJson {
    provider_user_id: String,
    internal_user_id: String,
}
