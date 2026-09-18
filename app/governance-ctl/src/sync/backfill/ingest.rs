//! Ingest operations for a backfill run: fetch a report's raw bytes, archive
//! them, parse + upsert, and (when an OTLP sink is configured) emit the parsed
//! rows as log records (ADR-0014).
//!
//! Split out of `backfill.rs` (#178). `ingest_day` and `ingest_seats` share
//! two helpers here: `archive_closure` (the archive closure that also captures
//! the raw bytes for later emission) and `emit_captured` (the OTLP emission).

use anyhow::Result;
use governance_copilot::{AppAuth, CopilotError, GithubClient, sync_day, sync_seats};
use tracing::info;

use crate::{emit::Sink, sync::config::Config};

/// Raw bytes captured from the archive closure, keyed by report, so they can
/// be re-parsed and emitted as OTLP records after the fetch/archive/upsert.
type CapturedBytes = std::sync::Arc<std::sync::Mutex<Vec<(String, Vec<u8>)>>>;

/// Build the archive closure that writes raw bytes through `cfg.archive` and
/// captures them (keyed by report) for later OTLP emission. The archive
/// closure is the one place the live bytes are visible to the CLI.
fn archive_closure(
    cfg: &Config,
    captured: &CapturedBytes,
) -> impl AsyncFn(&str, &[u8]) -> governance_copilot::Result<()> + 'static {
    let archive = cfg.archive.clone();
    let cap = std::sync::Arc::clone(captured);
    async move |key: &str, bytes: &[u8]| {
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
        archive
            .write(key, bytes)
            .await
            .map_err(|e| CopilotError::Archive(format!("{e:#}")))
    }
}

/// Emit the captured raw bytes as OTLP log records (ADR-0014) when a sink is
/// configured. No-op when `sink` is `None`. Clones out of the lock so the
/// guard is dropped before any await.
async fn emit_captured(
    sink: Option<&Sink>,
    captured: &CapturedBytes,
    cfg: &Config,
    day: &str,
) -> Result<()> {
    if let Some(sink) = sink {
        let captured = captured
            .lock()
            .map_err(|_| anyhow::anyhow!("capture lock poisoned while emitting OTLP records"))?
            .clone();
        for (report, bytes) in &captured {
            let rows = governance_copilot::parse_report_rows(report, bytes, day)?;
            sink.emit_rows(&cfg.tenant_id, &cfg.org, &rows).await?;
        }
    }
    Ok(())
}

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
    let captured: CapturedBytes = std::sync::Arc::default();
    let archive_fn = archive_closure(cfg, &captured);

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

    emit_captured(sink, &captured, cfg, day).await?;

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
pub(super) async fn ingest_seats(
    client: &GithubClient,
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
    snapshot_day: &str,
    sink: Option<&Sink>,
) -> Result<governance_copilot::ReportOutcome> {
    let auth = AppAuth::new(cfg.app_id.clone(), cfg.private_key.clone(), client);
    let captured: CapturedBytes = std::sync::Arc::default();
    let archive_fn = archive_closure(cfg, &captured);

    let outcome = sync_seats(
        client,
        pool,
        &auth,
        &cfg.tenant_id,
        &cfg.org,
        snapshot_day,
        archive_fn,
    )
    .await?;

    emit_captured(sink, &captured, cfg, snapshot_day).await?;

    info!(
        report = outcome.report,
        day = outcome.day,
        status = outcome.status,
        count = outcome.record_count,
        "ingested seat snapshot"
    );
    Ok(outcome)
}
