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
mod directory;
mod emit;
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
    /// Export the expected counts from `ingest_manifests` (plus per-table
    /// telemetry counts) as JSON -- the export the authz-side `verify-counts`
    /// CLI consumes (ADR-0014 cutover, lightbridge-authz#588).
    ExportCounts,
    /// Governance-side no-loss bar: verify the S3 raw archive is complete
    /// against `ingest_manifests` by parsing each archived (day, report) back
    /// and comparing counts. Exits non-zero on any mismatch so the cutover
    /// blocks loudly (no tables dropped).
    VerifyCounts,
    /// Drop the governance telemetry tables after counts are asserted
    /// (ADR-0014 cutover). Requires `--confirm` and a clean
    /// `verify-counts`; a count mismatch blocks the drop.
    Decommission {
        /// Acknowledge that dropping the telemetry tables is destructive and
        /// coordinated with the authz-side count assertions.
        #[arg(long)]
        confirm: bool,
        /// Also drop the SHARED telemetry tables
        /// (`executions`/`model_calls`/`tool_calls`), which the Foundry and
        /// redact connectors also write. Their no-loss bar is the authz-side
        /// `verify-counts`, not the governance-side verify here, so this must
        /// be an explicit flag -- never a default.
        #[arg(long)]
        include_shared_tables: bool,
    },
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
        /// Path to the directory file: a single JSON array of
        /// `{provider_user_id, internal_user_id}` objects, or one such array
        /// (or object) per line. Blank lines are ignored.
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
            let sink = emit::Sink::from_env()?;
            let result = sync::run_backfill(&client, &pool, &cfg, sink.as_ref()).await?;
            if let Some(endpoint) = metrics::endpoint_from_env() {
                metrics::push_run_metrics(
                    &endpoint,
                    "sync",
                    &result.outcomes,
                    result.covered as u64,
                )
                .await;
            }
            // AC 7: surface the OTLP emit outcome -- accepted rows by report
            // and any partial accept as an error metric -- so a rejected
            // record is never silently swallowed.
            if let Some(sink) = &sink {
                let (accepted, rejected) = sink.stats();
                if let Some(endpoint) = metrics::endpoint_from_env() {
                    metrics::push_emit_metrics(&endpoint, &accepted, rejected).await;
                }
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
            let sink = emit::Sink::from_env()?;
            let outcomes = sync::run_sync_day(&client, &pool, &cfg, &day, sink.as_ref()).await?;
            if let Some(endpoint) = metrics::endpoint_from_env() {
                metrics::push_run_metrics(&endpoint, "sync_day", &outcomes, 1).await;
            }
            if let Some(sink) = &sink {
                let (accepted, rejected) = sink.stats();
                if let Some(endpoint) = metrics::endpoint_from_env() {
                    metrics::push_emit_metrics(&endpoint, &accepted, rejected).await;
                }
            }
        }
        Command::Replay { from, to } => {
            let cfg = sync::Config::from_env().await?;
            let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
            let sink = emit::Sink::from_env()?;
            sync::run_replay(&pool, &cfg, &from, &to, sink.as_ref()).await?;
            if let Some(sink) = &sink {
                let (accepted, rejected) = sink.stats();
                if let Some(endpoint) = metrics::endpoint_from_env() {
                    metrics::push_emit_metrics(&endpoint, &accepted, rejected).await;
                }
            }
        }
        Command::Verify => {
            let cfg = sync::Config::from_env().await?;
            let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
            let mismatch = sync::run_verify(&pool, &cfg).await?;
            if let Some(endpoint) = metrics::endpoint_from_env() {
                metrics::push_verify_metrics(&endpoint, mismatch).await;
            }
        }
        Command::ExportCounts => {
            let cfg = sync::Config::from_env().await?;
            let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
            let export = sync::export_counts(&pool, &cfg).await?;
            println!("{}", serde_json::to_string_pretty(&export)?);
        }
        Command::VerifyCounts => {
            let cfg = sync::Config::from_env().await?;
            let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
            let mismatches = sync::verify_archive_counts(&pool, &cfg).await?;
            for m in &mismatches {
                tracing::warn!(
                    day = m.day,
                    report = m.report,
                    expected = m.expected,
                    actual = m.actual,
                    "archive/manifest count mismatch"
                );
            }
            if !mismatches.is_empty() {
                anyhow::bail!(
                    "verify-counts: {} count mismatch(es) between the archive and \
                     ingest_manifests; the cutover is blocked (no tables dropped)",
                    mismatches.len()
                );
            }
            tracing::info!("verify-counts: archive matches ingest_manifests; cutover may proceed");
        }
        Command::Decommission {
            confirm,
            include_shared_tables,
        } => {
            let cfg = sync::Config::from_env().await?;
            let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
            let dropped = sync::decommission(&pool, &cfg, confirm, include_shared_tables).await?;
            tracing::info!(tables = ?dropped, "governance telemetry tables dropped");
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
                anyhow::bail!(
                    "verify-attribution: unattributed executions present; attribution is incomplete"
                );
            }
        }
        Command::IdentitySync {
            tenant,
            provider,
            file,
        } => {
            let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
            let entries = directory::read_directory(&file)?;
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
