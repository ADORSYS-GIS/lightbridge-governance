//! The `export-counts` operator: read the expected per-`(day, report)` counts
//! from `ingest_manifests` (plus per-table telemetry counts) and emit them as
//! JSON in the authz `verify-counts` CLI's `VerifyManifest` shape.

use anyhow::{Context, Result};
use tracing::warn;

use super::{CountExport, ExpectedCount, SeatExpectation, refuse_during_freeze};
use crate::sync::config::Config;

/// Read the expected Copilot `(day, report)` counts from `ingest_manifests`
/// for `(tenant, provider, org)`, plus the per-table telemetry counts, in the
/// authz `VerifyManifest` shape.
pub async fn export_counts(pool: &cratestack::sqlx::PgPool, cfg: &Config) -> Result<CountExport> {
    refuse_during_freeze(cfg)?;
    let manifests: Vec<(chrono::NaiveDate, String, i64)> = cratestack::sqlx::query_as(
        "SELECT report_day::date, report_type, record_count \
         FROM ingest_manifests \
         WHERE tenant_id = $1 AND provider = $2 AND scope_id = $3 \
         ORDER BY report_day ASC, report_type ASC",
    )
    .bind(&cfg.tenant_id)
    .bind("github_copilot")
    .bind(&cfg.org)
    .fetch_all(pool)
    .await
    .context("reading ingest_manifests for count export")?;

    let mut day_facts = Vec::new();
    let mut seat_snapshots = Vec::new();
    for (day, report, expected) in manifests {
        match report.as_str() {
            // The three day-fact reports land in `usage_day_facts`.
            "organization-1-day" | "users-1-day" | "repos-1-day" => {
                day_facts.push(ExpectedCount {
                    day: day.to_string(),
                    report,
                    expected,
                });
            }
            // `billing-seats` lands in `usage_seat_snapshots`.
            "billing-seats" => {
                seat_snapshots.push(SeatExpectation {
                    day: day.to_string(),
                    expected,
                });
            }
            // `user-teams-1-day` is refused by the authz receiver (RFC-0001
            // known-issue #1), so it is not present in the usage store and is
            // not asserted.
            "user-teams-1-day" => {}
            other => {
                warn!(
                    report = other,
                    "unknown report in ingest_manifests; skipping its count assertion"
                );
            }
        }
    }

    let executions = count_table(pool, &cfg.tenant_id, "executions").await?;
    let model_calls = count_children(pool, &cfg.tenant_id, "model_calls").await?;
    let tool_calls = count_children(pool, &cfg.tenant_id, "tool_calls").await?;

    Ok(CountExport {
        day_facts,
        seat_snapshots,
        executions,
        model_calls,
        tool_calls,
    })
}

/// Count the rows of one telemetry table for a tenant. The execution/
/// model-call/tool-call tables carry `tenant_id` on every row (ADR-0001).
async fn count_table(pool: &cratestack::sqlx::PgPool, tenant_id: &str, table: &str) -> Result<i64> {
    let sql = format!("SELECT count(*) FROM {table} WHERE tenant_id = $1");
    let (n,): (i64,) = cratestack::sqlx::query_as(&sql)
        .bind(tenant_id)
        .fetch_one(pool)
        .await
        .with_context(|| format!("counting {table}"))?;
    Ok(n)
}

/// Count the rows of a child telemetry table (`model_calls`/`tool_calls`) for
/// a tenant. These tables carry no `tenant_id` of their own (ADR-0001's column
/// lives on the parent `executions`), so they are counted through a join to
/// their parent execution.
async fn count_children(
    pool: &cratestack::sqlx::PgPool,
    tenant_id: &str,
    table: &str,
) -> Result<i64> {
    let sql = format!(
        "SELECT count(*) FROM {table} c \
         JOIN executions e ON c.execution_id = e.id WHERE e.tenant_id = $1"
    );
    let (n,): (i64,) = cratestack::sqlx::query_as(&sql)
        .bind(tenant_id)
        .fetch_one(pool)
        .await
        .with_context(|| format!("counting {table}"))?;
    Ok(n)
}
