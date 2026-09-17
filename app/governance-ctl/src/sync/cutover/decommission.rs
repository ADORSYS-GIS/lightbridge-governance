//! The `decommission` operator: drop the governance telemetry tables after
//! counts are asserted (ADR-0014 cutover). Requires `--confirm` and a clean
//! `verify-counts`; a count mismatch blocks the drop.

use anyhow::{Context, Result};
use tracing::{info, warn};

use super::{TELEMETRY_TABLES, refuse_during_freeze, verify_archive_counts};
use crate::sync::config::Config;

/// Drop the governance telemetry tables (AC 4). Gated on two things:
///
/// 1. [`verify_archive_counts`] must pass -- a count mismatch blocks the
///    cutover loudly and no table is dropped.
/// 2. `confirm` must be `true` -- dropping tables is destructive and
///    coordinated with the authz-side count assertions, so it must be an
///    explicit operator action, never a default.
///
/// Returns the list of tables dropped.
pub async fn decommission(
    pool: &cratestack::sqlx::PgPool,
    cfg: &Config,
    confirm: bool,
) -> Result<Vec<String>> {
    if !confirm {
        anyhow::bail!(
            "decommission refused: pass --confirm to drop the governance telemetry tables \
             (this is destructive and coordinated with the authz-side count assertions)"
        );
    }

    // The no-loss bar is meaningless while the write path (and therefore
    // `ingest_manifests`) is frozen -- see `refuse_during_freeze`.
    refuse_during_freeze(cfg)?;

    let mismatches = verify_archive_counts(pool, cfg).await?;
    if !mismatches.is_empty() {
        for m in &mismatches {
            warn!(
                day = m.day,
                report = m.report,
                expected = m.expected,
                actual = m.actual,
                "count mismatch blocks decommission"
            );
        }
        anyhow::bail!(
            "decommission blocked: {} count mismatch(es) between the archive and \
             ingest_manifests; no table dropped (no-loss bar, #167)",
            mismatches.len()
        );
    }

    // `executions`/`model_calls`/`tool_calls` are the shared normalized
    // telemetry model written by EVERY push connector (Foundry, redact), not
    // just Copilot. The governance-side `verify_archive_counts` above only
    // verifies the Copilot S3 archive against `ingest_manifests` -- it does
    // NOT verify these tables' migration. Their no-loss bar is the authz-side
    // `verify-counts` CLI (which consumes `export-counts`). Dropping them is
    // only safe once that authz-side assertion has confirmed the usage store
    // matches; this warning is the operator's explicit acknowledgement.
    warn!(
        "decommission is about to drop the SHARED telemetry tables \
         (executions/model_calls/tool_calls), which the Foundry and redact connectors also \
         write. Confirm the authz-side verify-counts asserted the usage store matches before \
         proceeding."
    );

    let mut dropped = Vec::new();
    for table in TELEMETRY_TABLES {
        let sql = format!("DROP TABLE IF EXISTS {table}");
        cratestack::sqlx::query(&sql)
            .execute(pool)
            .await
            .with_context(|| format!("dropping {table}"))?;
        dropped.push((*table).to_owned());
        info!(table, "dropped governance telemetry table");
    }
    Ok(dropped)
}
