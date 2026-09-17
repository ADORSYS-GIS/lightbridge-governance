//! Telemetry ingest: batch upsert of executions, model calls, and tool calls (#30).
//!
//! All writes are idempotent on `(trace_id, span_id)` -- reprocessing the same
//! telemetry must not change row counts *or* the costs stored on first write
//! (a pricing change re-prices future ingests without rewriting history). The
//! whole batch runs in one transaction, so a partial failure rolls back
//! everything rather than leaving an execution with half its children.
//!
//! `tenant_id` and `integration_id` are derived from the authenticated
//! credential and stamped by Authorino, never read from the telemetry body
//! (RFC-0002's trust boundary).
//!
//! The split follows the ingest pipeline's seams: [`types`] (input model and
//! validation), [`pricing`] (cost lookup), and [`upsert`] (the transactional
//! writes). This module orchestrates them.

mod pricing;
mod retry;
#[cfg(test)]
mod tests;
mod types;
mod upsert;

use cratestack::{cratestack_error_from_sqlx, sqlx};
use pricing::calculate_model_cost;
use retry::{DEADLOCK_RETRY_BASE_MS, MAX_DEADLOCK_RETRIES, is_deadlock};
use sqlx::PgPool;
pub use types::{ExecutionInput, IngestResult, ModelCallInput, ToolCallInput};
use types::{deterministic_id, validate_input};
use upsert::{upsert_execution, upsert_model_call, upsert_tool_call};

use crate::{Error, Result};

/// Ingests normalized telemetry from a push connector.
///
/// The whole batch runs in a single transaction. All writes are idempotent on
/// `(trace_id, span_id)`; on conflict, mutable fields are refreshed but costs
/// are **never** overwritten -- history stays stable once written. `tenant_id`
/// and `integration_id` are trusted (Authorino-stamped), never from the body.
/// On success, the integration's `last_telemetry_at` is advanced.
///
/// # Errors
///
/// Returns [`Error::Validation`] if any input is malformed, or
/// [`Error::Storage`] if the database operation fails. The caller should treat
/// a storage error as a transient failure and retry (the transaction means a
/// retry is safe).
pub async fn ingest_telemetry(
    pool: &PgPool,
    tenant_id: &str,
    integration_id: &str,
    provider: &str,
    executions: &[ExecutionInput],
) -> Result<IngestResult> {
    validate_input(executions)?;

    let mut attempt = 0;
    loop {
        match ingest_telemetry_inner(pool, tenant_id, integration_id, provider, executions).await {
            Ok(result) => return Ok(result),
            Err(Error::Storage(e)) if is_deadlock(&e) && attempt < MAX_DEADLOCK_RETRIES => {
                attempt += 1;
                let delay = DEADLOCK_RETRY_BASE_MS * 2u64.pow(attempt - 1);
                tracing::warn!(
                    attempt,
                    delay_ms = delay,
                    integration_id,
                    "deadlock detected, retrying ingest"
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn ingest_telemetry_inner(
    pool: &PgPool,
    tenant_id: &str,
    integration_id: &str,
    provider: &str,
    executions: &[ExecutionInput],
) -> Result<IngestResult> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;

    let mut result = IngestResult {
        executions_upserted: 0,
        model_calls_upserted: 0,
        tool_calls_upserted: 0,
        identity_mismatch_detection_failed: false,
        identity_mismatches: 0,
    };

    // Resolve the integration's bound identity once per batch (#35).
    // Identity comes from the ingest token, not from the payload's user.email
    // (which is self-asserted and may be absent).
    let internal_user_id =
        crate::identity::get_integration_identity(pool, tenant_id, integration_id).await?;

    // Check for mismatches between token identity and payload emails (#35).
    // Batch query to avoid N+1 problem.
    let payload_emails: Vec<Option<&str>> =
        executions.iter().map(|e| e.user_email.as_deref()).collect();
    let (token_identity, mismatches, query_failed) = crate::identity::check_email_mismatches(
        pool,
        tenant_id,
        provider,
        internal_user_id.as_deref(),
        &payload_emails,
    )
    .await;

    // Store the query failure flag in the result for the app layer to handle metrics.
    result.identity_mismatch_detection_failed = query_failed;

    for (execution, mismatch) in executions.iter().zip(mismatches.iter()) {
        let execution_id = deterministic_id("exec", &execution.trace_id, &execution.span_id);

        if *mismatch {
            result.identity_mismatches += 1;
            // Log mismatch without PII: only log that a mismatch occurred, not the email itself.
            tracing::warn!(
                tenant_id = %tenant_id,
                integration_id = %integration_id,
                provider = %provider,
                internal_user_id = ?token_identity,
                "identity mismatch: payload email does not match token-derived identity"
            );
        }

        // Compute each model call's cost once, reuse it for both the child row
        // and the execution total. The pricing lookup is cheap but not free --
        // never query it twice for the same call (the pre-fix code did).
        // A call whose cost is unknown (missing token counts, or no pricing
        // row) makes the whole execution's estimate unknown too: summing the
        // known costs would silently understate, which reads as "cheaper than
        // it was" on a dashboard.
        let mut model_call_costs = Vec::with_capacity(execution.model_calls.len());
        let mut total_cost: Option<i64> = Some(0);
        for model_call in &execution.model_calls {
            let cost = calculate_model_cost(&mut tx, model_call).await?;
            if let Some(known) = cost {
                if let Some(total) = &mut total_cost {
                    *total += known.0;
                }
            } else {
                total_cost = None;
            }
            model_call_costs.push(cost);
        }

        upsert_execution(
            &mut tx,
            &execution_id,
            tenant_id,
            integration_id,
            provider,
            execution,
            total_cost,
            internal_user_id.as_deref(),
        )
        .await?;

        result.executions_upserted += 1;

        for (model_call, cost) in execution.model_calls.iter().zip(model_call_costs) {
            let model_call_id = deterministic_id("mc", &model_call.trace_id, &model_call.span_id);
            upsert_model_call(&mut tx, &model_call_id, &execution_id, model_call, cost).await?;
            result.model_calls_upserted += 1;
        }

        for tool_call in &execution.tool_calls {
            let tool_call_id = deterministic_id("tc", &tool_call.trace_id, &tool_call.span_id);
            upsert_tool_call(&mut tx, &tool_call_id, &execution_id, tool_call).await?;
            result.tool_calls_upserted += 1;
        }
    }

    // Reflect that this integration has (successfully) delivered telemetry.
    // Only the integration the Authorino-stamped header names is touched, and
    // only within the stamped tenant (tenant_id on every query).
    sqlx::query(
        "UPDATE integrations SET last_telemetry_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(integration_id)
    .bind(tenant_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;

    tx.commit()
        .await
        .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;

    Ok(result)
}
