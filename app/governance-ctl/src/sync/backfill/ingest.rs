//! The per-day and per-run ingestion steps: `ingest_day` fetches + archives +
//! emits one day's reports; `ingest_seats` snapshots the org's seats once per
//! run. Both archive raw bytes and, when an OTLP sink is configured, re-parse
//! and emit them as ADR-0014 day-grain log records.

use anyhow::Result;
use governance_copilot::{AppAuth, CopilotError, GithubClient, sync_day, sync_seats};
use tracing::info;

use crate::{emit::Sink, sync::config::Config};

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
pub(crate) async fn ingest_day(
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
        // Only buffer the raw bytes when a sink will re-parse and emit them;
        // on the common non-sink path (Postgres authoritative) there is no
        // reason to hold a copy of every report in memory.
        if sink.is_some() {
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
        }
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
        cfg.freeze_writes,
    )
    .await?;

    if let Some(sink) = sink {
        // Clone out of the lock so the guard is dropped before any await.
        let captured = captured
            .lock()
            .map_err(|_| anyhow::anyhow!("capture lock poisoned while emitting OTLP records"))?
            .clone();
        // Parse every report for the day, then emit them as one batch so the
        // org-level cost can be aggregated from the user rows (the org report
        // carries no cost) and natural-key collisions are detected across the
        // whole day (RFC-0001 known-issues #1/#5).
        let mut all_rows = Vec::new();
        for (report, bytes) in &captured {
            all_rows.push(governance_copilot::parse_report_rows(report, bytes, day)?);
        }
        sink.emit_rows(&cfg.tenant_id, &cfg.org, &all_rows, cfg.freeze_writes)
            .await?;
    } else if cfg.freeze_writes {
        // A freeze with no sink would silently write nothing -- the whole
        // point of the cutover is that the OTLP sink is the write path. Fail
        // loudly rather than drop the day's data on the floor.
        anyhow::bail!(
            "CUTOVER_FREEZE_WRITES is set but no OTLP sink is configured \
             (OTEL_EXPORTER_OTLP_ENDPOINT); refusing to ingest day {day} with no write path"
        );
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
/// ONCE per `sync` run by `run_backfill_at` -- never per backfilled
/// day, never from `run_sync_day` -- see `governance_copilot::sync_seats`'s
/// doc comment for why looping this would fabricate a seat history that was
/// never actually observed.
pub(crate) async fn ingest_seats(
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
        // Only buffer the raw bytes when a sink will re-parse and emit them.
        if sink.is_some() {
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
        }
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
        cfg.freeze_writes,
    )
    .await?;

    if let Some(sink) = sink {
        // Clone out of the lock so the guard is dropped before any await.
        let captured = captured
            .lock()
            .map_err(|_| anyhow::anyhow!("capture lock poisoned while emitting OTLP records"))?
            .clone();
        let mut all_rows = Vec::new();
        for (report, bytes) in &captured {
            all_rows.push(governance_copilot::parse_report_rows(
                report,
                bytes,
                snapshot_day,
            )?);
        }
        sink.emit_rows(&cfg.tenant_id, &cfg.org, &all_rows, cfg.freeze_writes)
            .await?;
    } else if cfg.freeze_writes {
        anyhow::bail!(
            "CUTOVER_FREEZE_WRITES is set but no OTLP sink is configured \
             (OTEL_EXPORTER_OTLP_ENDPOINT); refusing to snapshot seats with no write path"
        );
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
