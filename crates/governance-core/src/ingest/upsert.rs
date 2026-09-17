//! Transactional upsert of executions, model calls, and tool calls.
//!
//! All writes are idempotent on `(trace_id, span_id)`: on conflict, mutable
//! fields are refreshed but costs are **never** overwritten -- history stays
//! stable once written.

use cratestack::{cratestack_error_from_sqlx, sqlx};
use sqlx::{Postgres, Transaction};

use super::types::{ExecutionInput, ModelCallInput, ToolCallInput};
use crate::{Error, MicroUsd, Result};

#[allow(clippy::too_many_arguments)]
pub(crate) async fn upsert_execution(
    tx: &mut Transaction<'_, Postgres>,
    execution_id: &str,
    tenant_id: &str,
    integration_id: &str,
    provider: &str,
    execution: &ExecutionInput,
    total_cost: Option<i64>,
    internal_user_id: Option<&str>,
) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO executions
           (id, tenant_id, integration_id, provider, trace_id, span_id,
            user_email, internal_user_id, started_at, duration_ms, estimated_cost_micro_usd,
            raw_backend, raw_schema_version)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NULL, 1)
           ON CONFLICT (trace_id, span_id) DO UPDATE SET
            tenant_id = EXCLUDED.tenant_id,
            integration_id = EXCLUDED.integration_id,
            provider = EXCLUDED.provider,
            user_email = EXCLUDED.user_email,
            internal_user_id = EXCLUDED.internal_user_id,
            duration_ms = EXCLUDED.duration_ms,
            -- cost is deliberately NOT refreshed: history stays stable once
            -- written (a pricing change re-prices future ingests only)
            updated_at = now()"#,
    )
    .bind(execution_id)
    .bind(tenant_id)
    .bind(integration_id)
    .bind(provider)
    .bind(&execution.trace_id)
    .bind(&execution.span_id)
    .bind(&execution.user_email)
    .bind(internal_user_id)
    .bind(execution.started_at)
    .bind(execution.duration_ms)
    .bind(total_cost)
    .execute(&mut **tx)
    .await
    .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;
    Ok(())
}

pub(crate) async fn upsert_model_call(
    tx: &mut Transaction<'_, Postgres>,
    model_call_id: &str,
    execution_id: &str,
    model_call: &ModelCallInput,
    cost: Option<MicroUsd>,
) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO model_calls
           (id, execution_id, trace_id, span_id, model, input_tokens,
            output_tokens, cost_micro_usd)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           ON CONFLICT (trace_id, span_id) DO UPDATE SET
            model = EXCLUDED.model,
            input_tokens = EXCLUDED.input_tokens,
            output_tokens = EXCLUDED.output_tokens,
            -- cost is deliberately NOT refreshed: history stays stable once
            -- written
            updated_at = now()"#,
    )
    .bind(model_call_id)
    .bind(execution_id)
    .bind(&model_call.trace_id)
    .bind(&model_call.span_id)
    .bind(&model_call.model)
    .bind(model_call.input_tokens)
    .bind(model_call.output_tokens)
    .bind(cost.map(|c| c.0))
    .execute(&mut **tx)
    .await
    .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;
    Ok(())
}

pub(crate) async fn upsert_tool_call(
    tx: &mut Transaction<'_, Postgres>,
    tool_call_id: &str,
    execution_id: &str,
    tool_call: &ToolCallInput,
) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO tool_calls
           (id, execution_id, trace_id, span_id, tool_name, duration_ms)
           VALUES ($1, $2, $3, $4, $5, $6)
           ON CONFLICT (trace_id, span_id) DO UPDATE SET
            tool_name = EXCLUDED.tool_name,
            duration_ms = EXCLUDED.duration_ms,
            updated_at = now()"#,
    )
    .bind(tool_call_id)
    .bind(execution_id)
    .bind(&tool_call.trace_id)
    .bind(&tool_call.span_id)
    .bind(&tool_call.tool_name)
    .bind(tool_call.duration_ms)
    .execute(&mut **tx)
    .await
    .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;
    Ok(())
}
