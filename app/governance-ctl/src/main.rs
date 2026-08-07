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
        // TODO(RFC-0001): dispatch once the connector lands.
        Command::Sync | Command::SyncDay { .. } | Command::Replay { .. } | Command::Status => {}
        Command::Verify => {
            let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
            let tenant = resolve_single_tenant(&pool).await?;
            let report = governance_core::identity::verify_attribution(&pool, &tenant).await?;
            for provider in &report.providers {
                tracing::info!(
                    provider = %provider.provider,
                    attributed = provider.attributed,
                    unattributed = provider.unattributed,
                    mismatched = provider.mismatched,
                    "verify: attribution"
                );
            }
            if report.has_unattributed() {
                anyhow::bail!("verify: unattributed executions present; attribution is incomplete");
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
