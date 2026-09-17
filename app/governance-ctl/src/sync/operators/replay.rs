//! The `replay` operator: replay a day range from the raw archive, without
//! calling GitHub at all. Under the ADR-0014 cutover it is the mechanism that
//! pushes the historical archive through the day-grain ingest sink.

use anyhow::{Context, Result};
use governance_copilot::{manifest_schema_version, replay_report};
use tracing::{info, warn};

use super::super::config::Config;
use crate::emit::Sink;

/// Replay a day range from the raw archive, without calling GitHub at all.
///
/// This is the recovery path for a parse/upsert bug (RFC-0001): the archive
/// holds the exact bytes a re-fetch would return, so the replay exercises the
/// same `replay_report` code path as live ingestion.
///
/// Under the ADR-0014 cutover (`cfg.freeze_writes`), replay is the mechanism
/// that pushes the historical archive through the authenticated day-grain
/// ingest API: it parses each archived report and emits it as OTLP log records
/// via `sink` (the true write path) instead of writing to Postgres. A freeze
/// with no sink fails loudly rather than silently writing nothing.
pub async fn run_replay(
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
    from: &str,
    to: &str,
    sink: Option<&Sink>,
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
        if cfg.freeze_writes {
            // Cutover path: emit the archived rows through the usage ingest
            // sink (the true write path), never to Postgres. All reports for
            // the day are parsed and emitted as one batch so the org-level
            // cost can be aggregated from the user rows (the org report
            // carries no cost) and natural-key collisions are detected
            // day-wide (RFC-0001 known-issues #1/#5).
            let sink = sink.context(
                "CUTOVER_FREEZE_WRITES is set but no OTLP sink is configured \
                 (OTEL_EXPORTER_OTLP_ENDPOINT); refusing to replay with no write path",
            )?;
            let mut all_rows = Vec::new();
            for key in keys {
                let report = key
                    .rsplit('/')
                    .next()
                    .and_then(|f| {
                        f.strip_suffix(".ndjson")
                            .or_else(|| f.strip_suffix(".json"))
                    })
                    .unwrap_or(&key)
                    .to_owned();
                let bytes = cfg.archive.read(&key).await?;
                // A schema bump invalidates old archives; surface it rather
                // than silently replaying into the new shape (SCHEMA_VERSION).
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
                all_rows.push(governance_copilot::parse_report_rows(&report, &bytes, &ds)?);
            }
            let n = sink
                .emit_rows(&cfg.tenant_id, &cfg.org, &all_rows, true)
                .await?;
            info!(day = ds, count = n, "replayed day via OTLP sink");
        } else {
            for key in keys {
                let report = key
                    .rsplit('/')
                    .next()
                    .and_then(|f| {
                        f.strip_suffix(".ndjson")
                            .or_else(|| f.strip_suffix(".json"))
                    })
                    .unwrap_or(&key)
                    .to_owned();
                let bytes = cfg.archive.read(&key).await?;
                // A schema bump invalidates old archives; surface it rather
                // than silently replaying into the new shape (SCHEMA_VERSION).
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
        }
        day = day + chrono::Days::new(1);
    }
    Ok(())
}
