//! The `copilot-sync` run path: read config from env, then call the connector
//! pipeline for the requested days. `sync` (backfill) reads the high-water mark
//! from `ingest_manifests` and ingests from the most recent day back up to 28
//! days; `sync-day` ingests one explicit day. `replay`/`verify`/`status` are
//! the archive-facing operators (S3 phase of #12).

use anyhow::{Context, Result};
use governance_copilot::{
    AppAuth, CopilotError, GithubClient, RawSecret, high_water_mark, manifest_schema_version,
    replay_report, sync_day, unmapped_user_count, verify_manifests,
};
use tracing::{info, warn};

use crate::archive::Archive;

/// Connector configuration, read from the environment.
#[derive(Debug, Clone)]
pub struct Config {
    pub tenant_id: String,
    pub org: String,
    pub app_id: String,
    pub private_key: RawSecret,
    /// Where raw report NDJSON is archived (S3 in production, RAW_DIR locally).
    /// Required: archiving is a stated AC of #12, not an optional extra.
    pub archive: Archive,
}

impl Config {
    /// Build from env. The private key is read from `GH_APP_PRIVATE_KEY_FILE`
    /// (a PEM path), never from an env value, and wrapped in `RawSecret` so it
    /// cannot be logged (never-log-a-secret rule).
    pub async fn from_env() -> Result<Self> {
        let tenant_id = std::env::var("TENANT_ID")
            .context("TENANT_ID is required (single-tenant deployment, ADR-0001)")?;
        let org = std::env::var("GH_ORG").unwrap_or_else(|_| "adorsys-gis".to_owned());
        let app_id = std::env::var("GH_APP_ID").context("GH_APP_ID is required")?;
        let key_path = std::env::var("GH_APP_PRIVATE_KEY_FILE")
            .context("GH_APP_PRIVATE_KEY_FILE is required (path to the App PEM)")?;
        let pem =
            std::fs::read_to_string(&key_path).with_context(|| format!("reading {key_path}"))?;
        let archive = Archive::from_env().await?.context(
            "no archive sink configured: set AWS_ACCESS_KEY_ID + \
                 AWS_SECRET_ACCESS_KEY (S3) or RAW_DIR (local); a run that \
                 archives nothing is a silent failure (#12)",
        )?;
        Ok(Self {
            tenant_id,
            org,
            app_id,
            private_key: RawSecret::new(pem),
            archive,
        })
    }
}

/// Ingest a single day, archiving raw NDJSON through the configured sink.
async fn ingest_day(
    client: &GithubClient,
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
    day: &str,
) -> Result<Vec<governance_copilot::ReportOutcome>> {
    let auth = AppAuth::new(cfg.app_id.clone(), cfg.private_key.clone(), client);
    let archive = cfg.archive.clone();
    let archive_fn = async |key: &str, bytes: &[u8]| {
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

/// Backfill: ingest from the high-water mark backwards up to 28 days, newest
/// first (a late-published report is picked up, and an outage self-heals).
/// Returns the per-report outcomes and the number of days covered.
pub async fn run_backfill(
    client: &GithubClient,
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
) -> Result<(Vec<governance_copilot::ReportOutcome>, usize)> {
    let hwm = high_water_mark(pool, &cfg.tenant_id, "github_copilot").await?;
    // Most recent complete day is "yesterday" UTC.
    let today = chrono::Utc::now().date_naive();
    let start = hwm.unwrap_or_else(|| today - chrono::Days::new(28));
    let start = (start + chrono::Days::new(1)).min(today); // resume after HWM, never beyond today
    let end = today;

    let mut days: Vec<chrono::NaiveDate> = Vec::new();
    let mut d = start;
    while d <= end {
        days.push(d);
        d = d + chrono::Days::new(1);
    }
    days.reverse(); // newest first so a late report lands quickly

    info!(start = %start, end = %end, n = days.len(), "backfill window");
    let mut all = Vec::new();
    let mut covered = 0usize;
    for day in days {
        let ds = day.format("%Y-%m-%d").to_string();
        match ingest_day(client, pool, cfg, &ds).await {
            Ok(outcomes) => {
                covered += 1;
                all.extend(outcomes);
            }
            Err(e) => warn!(day = ds, error = %e, "day failed; continuing backfill"),
        }
    }
    Ok((all, covered))
}

/// Ingest one explicit day. Idempotent.
pub async fn run_sync_day(
    client: &GithubClient,
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
    day: &str,
) -> Result<Vec<governance_copilot::ReportOutcome>> {
    // Validate the day string early so a typo fails before any network call.
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .with_context(|| format!("invalid day {day:?}, want YYYY-MM-DD"))?;
    ingest_day(client, pool, cfg, day).await
}

/// Replay a day range from the raw archive, without calling GitHub at all.
///
/// This is the recovery path for a parse/upsert bug (RFC-0001): the archive
/// holds the exact bytes a re-fetch would return, so the replay exercises the
/// same `replay_report` code path as live ingestion.
pub async fn run_replay(
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
    from: &str,
    to: &str,
) -> Result<()> {
    let from = chrono::NaiveDate::parse_from_str(from, "%Y-%m-%d")
        .with_context(|| format!("invalid day {from:?}, want YYYY-MM-DD"))?;
    let to = chrono::NaiveDate::parse_from_str(to, "%Y-%m-%d")
        .with_context(|| format!("invalid day {to:?}, want YYYY-MM-DD"))?;
    if to < from {
        anyhow::bail!("replay range is inverted: {from} > {to}");
    }

    let mut day = from;
    while day <= to {
        let ds = day.format("%Y-%m-%d").to_string();
        let keys = cfg.archive.list_day(&cfg.org, &ds).await?;
        if keys.is_empty() {
            info!(day = ds, "no archived reports for day; nothing to replay");
        }
        for key in keys {
            let report = key
                .rsplit('/')
                .next()
                .and_then(|f| f.strip_suffix(".ndjson"))
                .unwrap_or(&key)
                .to_owned();
            let bytes = cfg.archive.read(&key).await?;
            // A schema bump invalidates old archives; surface it rather than
            // silently replaying into the new shape (SCHEMA_VERSION).
            if let Some(version) = manifest_schema_version(
                pool,
                &cfg.tenant_id,
                "github_copilot",
                &cfg.org,
                &report,
                &ds,
            )
            .await?
                && version < governance_copilot::SCHEMA_VERSION
            {
                warn!(
                    report = report,
                    day = ds,
                    archived_schema = version,
                    current_schema = governance_copilot::SCHEMA_VERSION,
                    "replaying archive written under an older schema"
                );
            }
            let n = replay_report(pool, &cfg.tenant_id, &cfg.org, &ds, &report, &bytes).await?;
            info!(report = report, day = ds, count = n, "replayed report");
        }
        day = day + chrono::Days::new(1);
    }
    Ok(())
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

/// Print connector status: last success, report age, unmapped users. Returns
/// `(age_days, unmapped_users)` for the metrics push.
pub async fn run_status(pool: &cratestack::sqlx::PgPool, cfg: &Config) -> Result<(i64, i64)> {
    let Some(last) = high_water_mark(pool, &cfg.tenant_id, "github_copilot").await? else {
        info!("no manifests yet; nothing has been ingested");
        return Ok((-1, -1));
    };
    let today = chrono::Utc::now().date_naive();
    let age = today.signed_duration_since(last).num_days();
    let unmapped = unmapped_user_count(pool, &cfg.tenant_id, &cfg.org, &last.to_string()).await?;
    info!(
        last = %last,
        age_days = age,
        unmapped_users = unmapped,
        "connector status"
    );
    Ok((age, unmapped))
}
