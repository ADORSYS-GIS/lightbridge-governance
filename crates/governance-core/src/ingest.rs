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

use chrono::{DateTime, Utc};
use cratestack::{cool_error_from_sqlx, sqlx};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};

use crate::{Error, MicroUsd, Result};

/// Normalized telemetry from a push connector.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecutionInput {
    pub trace_id: String,
    pub span_id: String,
    pub user_email: Option<String>,
    pub started_at: DateTime<Utc>,
    pub duration_ms: i64,
    pub model_calls: Vec<ModelCallInput>,
    pub tool_calls: Vec<ToolCallInput>,
}

/// One LLM call within an execution.
///
/// `input_tokens`/`output_tokens` are optional: a provider may omit token
/// counts, in which case the call is still recorded but its cost is stored as
/// *unknown* (`None`), never a zero -- a zero is indistinguishable from "free"
/// on a dashboard (story #31 AC6).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelCallInput {
    pub trace_id: String,
    pub span_id: String,
    pub model: String,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

/// One tool invocation within an execution.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCallInput {
    pub trace_id: String,
    pub span_id: String,
    pub tool_name: String,
    pub duration_ms: i64,
}

/// Result of an ingest operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IngestResult {
    pub executions_upserted: i64,
    pub model_calls_upserted: i64,
    pub tool_calls_upserted: i64,
}

/// Derives a deterministic id from `(trace_id, span_id)` so the same
/// execution always maps to the same row -- critical for idempotent upsert,
/// since child rows (model_calls, tool_calls) reference this id as their FK.
fn deterministic_id(prefix: &str, trace_id: &str, span_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(trace_id.as_bytes());
    hasher.update(b":");
    hasher.update(span_id.as_bytes());
    let hash = hex::encode(hasher.finalize());
    format!("{prefix}-{}", &hash[..24])
}

/// Validates caller-supplied telemetry before any persistence happens.
///
/// The idempotency key is `(trace_id, span_id)`; every row must carry a
/// non-empty one or the upsert's `ON CONFLICT` has nothing reliable to target.
/// Token counts and durations must be non-negative (a negative token count is
/// malformed input, not a "zero-cost call").
fn validate_input(executions: &[ExecutionInput]) -> Result<()> {
    let reject = |message: String| Err(Error::Validation(message));

    for execution in executions {
        if execution.trace_id.is_empty() || execution.span_id.is_empty() {
            return reject("trace_id and span_id are required on every execution".to_owned());
        }
        if execution.duration_ms < 0 {
            return reject(format!(
                "duration_ms must be non-negative, got {}",
                execution.duration_ms
            ));
        }
        for model_call in &execution.model_calls {
            if model_call.trace_id.is_empty() || model_call.span_id.is_empty() {
                return reject("trace_id and span_id are required on every model call".to_owned());
            }
            // A missing token count is "unknown", which is stored as unknown
            // cost -- not malformed. A *negative* count is malformed input,
            // not a zero-cost call.
            if model_call.input_tokens.is_some_and(|n| n < 0)
                || model_call.output_tokens.is_some_and(|n| n < 0)
            {
                return reject(format!(
                    "token counts must be non-negative, got input={:?} output={:?} for model {}",
                    model_call.input_tokens, model_call.output_tokens, model_call.model
                ));
            }
        }
        for tool_call in &execution.tool_calls {
            if tool_call.trace_id.is_empty() || tool_call.span_id.is_empty() {
                return reject("trace_id and span_id are required on every tool call".to_owned());
            }
            if tool_call.duration_ms < 0 {
                return reject(format!(
                    "tool duration_ms must be non-negative, got {}",
                    tool_call.duration_ms
                ));
            }
        }
    }
    Ok(())
}

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

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| Error::Storage(cool_error_from_sqlx(e)))?;

    let mut result = IngestResult {
        executions_upserted: 0,
        model_calls_upserted: 0,
        tool_calls_upserted: 0,
    };

    // Resolve the integration's bound identity once per batch (#35).
    // Identity comes from the ingest token, not from the payload's user.email
    // (which is self-asserted and may be absent).
    let internal_user_id =
        crate::identity::get_integration_identity(pool, tenant_id, integration_id).await?;

    for execution in executions {
        let execution_id = deterministic_id("exec", &execution.trace_id, &execution.span_id);

        // Check for mismatch between token identity and payload email (#35).
        let resolution = crate::identity::check_email_mismatch(
            pool,
            tenant_id,
            provider,
            internal_user_id.as_deref(),
            execution.user_email.as_deref(),
        )
        .await?;

        if resolution.mismatch {
            tracing::warn!(
                tenant_id = %tenant_id,
                integration_id = %integration_id,
                provider = %provider,
                payload_email = ?execution.user_email,
                internal_user_id = ?internal_user_id,
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
    .map_err(|e| Error::Storage(cool_error_from_sqlx(e)))?;

    tx.commit()
        .await
        .map_err(|e| Error::Storage(cool_error_from_sqlx(e)))?;

    Ok(result)
}

#[allow(clippy::too_many_arguments)]
async fn upsert_execution(
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
    .map_err(|e| Error::Storage(cool_error_from_sqlx(e)))?;
    Ok(())
}

async fn upsert_model_call(
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
    .map_err(|e| Error::Storage(cool_error_from_sqlx(e)))?;
    Ok(())
}

async fn upsert_tool_call(
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
    .map_err(|e| Error::Storage(cool_error_from_sqlx(e)))?;
    Ok(())
}

/// Calculates the cost of a model call from the pricing table.
///
/// Uses the most recent pricing entry for the model that is effective at the
/// time of the call. Returns `None` -- cost *unknown* -- when the call has no
/// token counts, or when no pricing exists for the model. Unknown is honest:
/// a zero would be indistinguishable from "free" on a dashboard (story #31
/// AC6). The call is still ingested, just unpriced.
async fn calculate_model_cost(
    tx: &mut Transaction<'_, Postgres>,
    model_call: &ModelCallInput,
) -> Result<Option<MicroUsd>> {
    let (Some(input_tokens), Some(output_tokens)) =
        (model_call.input_tokens, model_call.output_tokens)
    else {
        // No token counts -> no way to price the call. Unknown, not zero.
        return Ok(None);
    };

    let pricing: Option<(i64, i64)> = sqlx::query_as(
        r#"SELECT input_per_million_micro_usd, output_per_million_micro_usd
           FROM model_pricing
           WHERE model = $1 AND effective_from <= now()
             AND (effective_to IS NULL OR effective_to > now())
           ORDER BY effective_from DESC
           LIMIT 1"#,
    )
    .bind(&model_call.model)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| Error::Storage(cool_error_from_sqlx(e)))?;

    let Some((input_rate, output_rate)) = pricing else {
        // No pricing row for this model -> cost unknown, not zero.
        return Ok(None);
    };

    // Compute in i128: token counts are attacker-controlled telemetry and the
    // rates are i64, so the product can overflow i64 and wrap silently in
    // release builds (overflow checks are off). The division happens before
    // narrowing, so the intermediate stays exact for any realistic input.
    let input_cost = (i128::from(input_tokens) * i128::from(input_rate)) / 1_000_000;
    let output_cost = (i128::from(output_tokens) * i128::from(output_rate)) / 1_000_000;

    Ok(Some(MicroUsd(
        i64::try_from(input_cost + output_cost).unwrap_or(i64::MAX),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_id_is_stable_across_calls() {
        let a = deterministic_id("exec", "trace-1", "span-1");
        let b = deterministic_id("exec", "trace-1", "span-1");
        assert_eq!(a, b, "same inputs must produce the same id");
    }

    #[test]
    fn deterministic_id_differs_for_different_inputs() {
        let a = deterministic_id("exec", "trace-1", "span-1");
        let b = deterministic_id("exec", "trace-1", "span-2");
        assert_ne!(a, b, "different span_ids must produce different ids");
    }

    #[test]
    fn deterministic_id_carries_the_prefix() {
        let id = deterministic_id("mc", "trace-1", "span-1");
        assert!(id.starts_with("mc-"), "id must carry the prefix, got {id}");
    }

    #[test]
    fn execution_input_serializes_correctly() {
        let input = ExecutionInput {
            trace_id: "trace-123".to_owned(),
            span_id: "span-456".to_owned(),
            user_email: Some("user@example.com".to_owned()),
            started_at: Utc::now(),
            duration_ms: 1500,
            model_calls: vec![ModelCallInput {
                trace_id: "trace-123".to_owned(),
                span_id: "mc-789".to_owned(),
                model: "claude-3-sonnet".to_owned(),
                input_tokens: Some(1000),
                output_tokens: Some(500),
            }],
            tool_calls: vec![],
        };

        let json = serde_json::to_string(&input).expect("serialize");
        assert!(json.contains("trace-123"));
        assert!(json.contains("claude-3-sonnet"));
    }

    #[test]
    fn ingest_result_serializes_correctly() {
        let result = IngestResult {
            executions_upserted: 5,
            model_calls_upserted: 10,
            tool_calls_upserted: 3,
        };

        let json = serde_json::to_string(&result).expect("serialize");
        assert!(json.contains("\"executions_upserted\":5"));
        assert!(json.contains("\"model_calls_upserted\":10"));
        assert!(json.contains("\"tool_calls_upserted\":3"));
    }

    fn valid_execution() -> ExecutionInput {
        ExecutionInput {
            trace_id: "trace-1".to_owned(),
            span_id: "span-1".to_owned(),
            user_email: None,
            started_at: Utc::now(),
            duration_ms: 1000,
            model_calls: vec![ModelCallInput {
                trace_id: "trace-1".to_owned(),
                span_id: "span-1:mc".to_owned(),
                model: "claude-3-sonnet".to_owned(),
                input_tokens: Some(10),
                output_tokens: Some(5),
            }],
            tool_calls: vec![],
        }
    }

    #[test]
    fn validation_accepts_well_formed_input() {
        assert!(validate_input(&[valid_execution()]).is_ok());
    }

    #[test]
    fn validation_rejects_empty_trace_id() {
        let mut execution = valid_execution();
        execution.trace_id.clear();
        assert!(matches!(
            validate_input(&[execution]),
            Err(Error::Validation(_))
        ));
    }

    #[test]
    fn validation_rejects_negative_duration() {
        let mut execution = valid_execution();
        execution.duration_ms = -1;
        assert!(matches!(
            validate_input(&[execution]),
            Err(Error::Validation(_))
        ));
    }

    #[test]
    fn validation_rejects_negative_token_counts() {
        let mut execution = valid_execution();
        execution.model_calls[0].input_tokens = Some(-1);
        assert!(matches!(
            validate_input(&[execution]),
            Err(Error::Validation(_))
        ));
    }

    #[test]
    fn validation_accepts_missing_token_counts() {
        // A missing token count is "unknown", not malformed -- the call is
        // stored with unknown cost (story #31 AC6), never rejected.
        let mut execution = valid_execution();
        execution.model_calls[0].input_tokens = None;
        execution.model_calls[0].output_tokens = None;
        assert!(validate_input(&[execution]).is_ok());
    }

    #[test]
    fn validation_rejects_negative_tool_duration() {
        let mut execution = valid_execution();
        execution.tool_calls.push(ToolCallInput {
            trace_id: "trace-1".to_owned(),
            span_id: "span-1:tc:0".to_owned(),
            tool_name: "bash".to_owned(),
            duration_ms: -5,
        });
        assert!(matches!(
            validate_input(&[execution]),
            Err(Error::Validation(_))
        ));
    }

    /// Runs `ingest_telemetry` against a real Postgres when `DATABASE_URL` is
    /// set (mirrors resolve.rs's gated integration test -- the migration runs
    /// inside `connected_pool`, so a fresh DB works too). Returns `None` when
    /// skipped, so the test is a genuine no-op (and reports green) without the
    /// env var, exactly like resolve's.
    ///
    /// The migration is serialized behind a process-wide lock: cratestack's
    /// migration is not safe to run from two threads at once (it races on
    /// CREATE TYPE), and cargo runs test fns in parallel within the process.
    /// A Tokio mutex rather than `std::sync::Mutex` so the guard is not held
    /// across an await.
    async fn connected_pool() -> Option<PgPool> {
        let database_url = std::env::var("DATABASE_URL").ok()?;
        let pool = PgPool::connect(&database_url).await.expect("connect");
        static MIGRATION_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        {
            let _guard = MIGRATION_LOCK.lock().await;
            crate::migrate::run(&pool).await.expect("migrate");
        }
        Some(pool)
    }

    /// Inserts the minimal tenant + application + environment + integration
    /// fixture `ingest_telemetry` needs (it writes
    /// `integrations.last_telemetry_at`, and integrations has FK constraints
    /// to applications and environments). `provider` is the integration's
    /// stored provider string (the data the ingest path dispatches on).
    async fn fixture(pool: &PgPool, provider: &str) -> (String, String) {
        let tenant_id = format!("tenant-{}", cuid::cuid2());
        let application_id = format!("app-{}", cuid::cuid2());
        let environment_id = format!("env-{}", cuid::cuid2());
        let integration_id = format!("integration-{}", cuid::cuid2());
        sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, $2)")
            .bind(&tenant_id)
            .bind("ingest-test-tenant")
            .execute(pool)
            .await
            .expect("insert tenant fixture");
        sqlx::query("INSERT INTO applications (id, tenant_id, name) VALUES ($1, $2, $3)")
            .bind(&application_id)
            .bind(&tenant_id)
            .bind("ingest-test-app")
            .execute(pool)
            .await
            .expect("insert application fixture");
        sqlx::query(
            "INSERT INTO environments (id, tenant_id, application_id, name) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&environment_id)
        .bind(&tenant_id)
        .bind(&application_id)
        .bind("dev")
        .execute(pool)
        .await
        .expect("insert environment fixture");
        sqlx::query(
            "INSERT INTO integrations (id, tenant_id, application_id, environment_id, provider, \
             credential_prefix, credential_hash, status, content_capture) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(&integration_id)
        .bind(&tenant_id)
        .bind(&application_id)
        .bind(&environment_id)
        .bind(provider)
        .bind("prefix")
        .bind("hash")
        .bind("active")
        .bind("none")
        .execute(pool)
        .await
        .expect("insert integration fixture");
        (tenant_id, integration_id)
    }

    /// Like `fixture`, but creates two integrations under the same tenant.
    /// Used by the two-providers test to verify cross-provider queries.
    async fn fixture_two_providers(
        pool: &PgPool,
        provider_a: &str,
        provider_b: &str,
    ) -> (String, String, String) {
        let tenant_id = format!("tenant-{}", cuid::cuid2());
        let application_id = format!("app-{}", cuid::cuid2());
        let environment_id = format!("env-{}", cuid::cuid2());
        let integration_a = format!("integration-{}", cuid::cuid2());
        let integration_b = format!("integration-{}", cuid::cuid2());
        sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, $2)")
            .bind(&tenant_id)
            .bind("ingest-test-tenant")
            .execute(pool)
            .await
            .expect("insert tenant fixture");
        sqlx::query("INSERT INTO applications (id, tenant_id, name) VALUES ($1, $2, $3)")
            .bind(&application_id)
            .bind(&tenant_id)
            .bind("ingest-test-app")
            .execute(pool)
            .await
            .expect("insert application fixture");
        sqlx::query(
            "INSERT INTO environments (id, tenant_id, application_id, name) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&environment_id)
        .bind(&tenant_id)
        .bind(&application_id)
        .bind("dev")
        .execute(pool)
        .await
        .expect("insert environment fixture");
        sqlx::query(
            "INSERT INTO integrations (id, tenant_id, application_id, environment_id, provider, \
             credential_prefix, credential_hash, status, content_capture) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(&integration_a)
        .bind(&tenant_id)
        .bind(&application_id)
        .bind(&environment_id)
        .bind(provider_a)
        .bind("prefix")
        .bind("hash")
        .bind("active")
        .bind("none")
        .execute(pool)
        .await
        .expect("insert integration fixture");
        sqlx::query(
            "INSERT INTO integrations (id, tenant_id, application_id, environment_id, provider, \
             credential_prefix, credential_hash, status, content_capture) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(&integration_b)
        .bind(&tenant_id)
        .bind(&application_id)
        .bind(&environment_id)
        .bind(provider_b)
        .bind("prefix")
        .bind("hash")
        .bind("active")
        .bind("none")
        .execute(pool)
        .await
        .expect("insert integration fixture");
        (tenant_id, integration_a, integration_b)
    }

    /// The idempotency contract, against the real database: reprocessing the
    /// same `(trace_id, span_id)` must not change row counts *or* the costs
    /// stored on first write. A pricing change must re-price future ingests,
    /// never rewrite history -- the single most important invariant of this
    /// module, and the one a pure-unit suite cannot see.
    #[tokio::test]
    async fn reprocessing_is_idempotent_and_preserves_cost_history() {
        let Some(pool) = connected_pool().await else {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        };
        let (tenant_id, integration_id) = fixture(&pool, "claude_code").await;

        let pricing = MicroUsd(7_000_000); // $7.00 per million tokens
        // A model name no other DB test prices, so parallel tests cannot
        // interleave a competing pricing row into this test's lookup.
        const MODEL: &str = "idem-sonnet";
        sqlx::query(
            "INSERT INTO model_pricing (id, model, input_per_million_micro_usd, \
             output_per_million_micro_usd, effective_from) \
             VALUES ($1, $2, $3, $4, now())",
        )
        .bind(format!("price-{}", cuid::cuid2()))
        .bind(MODEL)
        .bind(pricing.0)
        .bind(pricing.0)
        .execute(&pool)
        .await
        .expect("insert pricing fixture");

        let mut execution = valid_execution(); // 10 in, 5 out
        execution.model_calls[0].model = MODEL.to_owned();
        let executions = vec![execution.clone()];
        let first = ingest_telemetry(
            &pool,
            &tenant_id,
            &integration_id,
            "claude_code",
            &executions,
        )
        .await
        .expect("first ingest succeeds");
        assert_eq!(
            (
                first.executions_upserted,
                first.model_calls_upserted,
                first.tool_calls_upserted
            ),
            (1, 1, 0)
        );

        let second = ingest_telemetry(
            &pool,
            &tenant_id,
            &integration_id,
            "claude_code",
            &executions,
        )
        .await
        .expect("reprocessing succeeds");
        assert_eq!(
            (
                second.executions_upserted,
                second.model_calls_upserted,
                second.tool_calls_upserted
            ),
            (1, 1, 0),
            "row counts must not change on reprocessing"
        );

        // The stored cost on the first write is what history must keep. A
        // future pricing change re-prices new rows, never existing ones.
        let (execution_id, execution_cost): (String, Option<i64>) = sqlx::query_as(
            "SELECT id, estimated_cost_micro_usd FROM executions \
             WHERE trace_id = $1 AND span_id = $2",
        )
        .bind("trace-1")
        .bind("span-1")
        .fetch_one(&pool)
        .await
        .expect("execution row exists");
        assert!(
            execution_id.starts_with("exec-"),
            "deterministic id must be stable"
        );

        let (model_call_cost,): (Option<i64>,) =
            sqlx::query_as("SELECT cost_micro_usd FROM model_calls WHERE execution_id = $1")
                .bind(&execution_id)
                .fetch_one(&pool)
                .await
                .expect("model call row exists");

        // $7.00 per million: 10 input tokens = 70, 5 output tokens = 35,
        // total 105 micro-USD for the execution. Two ingests must agree.
        assert_eq!(model_call_cost, Some(105), "10*7 + 5*7");
        assert_eq!(
            execution_cost,
            Some(105),
            "execution total = sum of model calls"
        );
    }

    /// Re-ingesting with a *changed* pricing row must re-price only new rows:
    /// the previously written cost_micro_usd stays put. This is what "history
    /// stays stable once written" means operationally.
    #[tokio::test]
    async fn a_pricing_change_does_not_rewrite_written_costs() {
        let Some(pool) = connected_pool().await else {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        };
        let (tenant_id, integration_id) = fixture(&pool, "claude_code").await;
        // A model name no other DB test prices, so parallel tests cannot
        // interleave a competing pricing row into this test's lookup.
        const MODEL: &str = "reprice-sonnet";

        async fn insert_price(pool: &PgPool, model: &str, id: &str, rate: i64) {
            sqlx::query(
                "INSERT INTO model_pricing (id, model, input_per_million_micro_usd, \
                 output_per_million_micro_usd, effective_from) \
                 VALUES ($1, $2, $3, $4, now())",
            )
            .bind(id)
            .bind(model)
            .bind(rate)
            .bind(rate)
            .execute(pool)
            .await
            .expect("insert pricing fixture");
        }
        insert_price(&pool, MODEL, &format!("price-{}", cuid::cuid2()), 7_000_000).await;
        let mut execution = valid_execution();
        execution.model_calls[0].model = MODEL.to_owned();
        let executions = vec![execution];
        ingest_telemetry(
            &pool,
            &tenant_id,
            &integration_id,
            "claude_code",
            &executions,
        )
        .await
        .expect("first ingest succeeds");

        // Pricing changes after the first write.
        insert_price(
            &pool,
            MODEL,
            &format!("price-{}", cuid::cuid2()),
            21_000_000,
        )
        .await;

        ingest_telemetry(
            &pool,
            &tenant_id,
            &integration_id,
            "claude_code",
            &executions,
        )
        .await
        .expect("reprocess succeeds");

        let (execution_cost,): (Option<i64>,) = sqlx::query_as(
            "SELECT estimated_cost_micro_usd FROM executions \
             WHERE trace_id = $1 AND span_id = $2",
        )
        .bind("trace-1")
        .bind("span-1")
        .fetch_one(&pool)
        .await
        .expect("execution row exists");
        assert_eq!(
            execution_cost,
            Some(105),
            "a pricing change must not rewrite already-stored costs"
        );
    }

    /// Story #31 AC6, negative case: a model call with *missing* token counts
    /// is stored with cost explicitly unknown (NULL), never a zero that a
    /// dashboard would read as "free".
    #[tokio::test]
    async fn missing_token_counts_are_stored_as_unknown_cost() {
        let Some(pool) = connected_pool().await else {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        };
        let (tenant_id, integration_id) = fixture(&pool, "claude_code").await;

        // No pricing row at all: the cost must come out unknown either way, so
        // the assertion isolates "missing tokens" from "missing pricing".
        // Unique ids: valid_execution() defaults to trace-1/span-1, which the
        // other DB tests also use -- parallel tests must not collide on rows.
        let mut execution = valid_execution();
        execution.trace_id = "trace-unknown-tokens".to_owned();
        execution.span_id = "span-unknown-tokens".to_owned();
        execution.model_calls[0].trace_id = "trace-unknown-tokens".to_owned();
        execution.model_calls[0].span_id = "span-unknown-tokens:mc".to_owned();
        execution.model_calls[0].input_tokens = None;
        execution.model_calls[0].output_tokens = None;
        let executions = vec![execution.clone()];

        let result = ingest_telemetry(
            &pool,
            &tenant_id,
            &integration_id,
            "claude_code",
            &executions,
        )
        .await
        .expect("ingest with missing tokens succeeds");

        assert_eq!(
            (result.executions_upserted, result.model_calls_upserted),
            (1, 1),
            "the call is still recorded, just unpriced"
        );

        let (execution_cost,): (Option<i64>,) = sqlx::query_as(
            "SELECT estimated_cost_micro_usd FROM executions WHERE trace_id = $1 AND span_id = $2",
        )
        .bind(&execution.trace_id)
        .bind(&execution.span_id)
        .fetch_one(&pool)
        .await
        .expect("execution row exists");
        assert_eq!(
            execution_cost, None,
            "execution cost must be unknown, not zero"
        );

        let (model_call_cost, input_tokens, output_tokens): (
            Option<i64>,
            Option<i64>,
            Option<i64>,
        ) = sqlx::query_as(
            "SELECT cost_micro_usd, input_tokens, output_tokens FROM model_calls \
                 WHERE trace_id = $1 AND span_id = $2",
        )
        .bind(&execution.model_calls[0].trace_id)
        .bind(&execution.model_calls[0].span_id)
        .fetch_one(&pool)
        .await
        .expect("model call row exists");
        assert_eq!(
            (model_call_cost, input_tokens, output_tokens),
            (None, None, None),
            "unknown tokens must yield unknown cost, never 0/0"
        );
    }

    /// Story #31 AC6, negative case: token counts *present* but no pricing row
    /// for the model is also cost unknown, not zero -- the previous
    /// `MicroUsd(0)` default was exactly the "zero is indistinguishable from
    /// free" hazard the story calls out.
    #[tokio::test]
    async fn missing_pricing_is_stored_as_unknown_not_zero() {
        let Some(pool) = connected_pool().await else {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        };
        let (tenant_id, integration_id) = fixture(&pool, "claude_code").await;

        // valid_execution() has tokens (Some(10)/Some(5)) but no pricing row
        // is inserted for its model -- a unique model name, so a parallel test
        // inserting "claude-3-sonnet" pricing cannot accidentally price it.
        let mut execution = valid_execution();
        execution.trace_id = "trace-no-pricing".to_owned();
        execution.span_id = "span-no-pricing".to_owned();
        execution.model_calls[0].trace_id = "trace-no-pricing".to_owned();
        execution.model_calls[0].span_id = "span-no-pricing:mc".to_owned();
        execution.model_calls[0].model = "never-priced-model".to_owned();
        let executions = vec![execution];
        let result = ingest_telemetry(
            &pool,
            &tenant_id,
            &integration_id,
            "claude_code",
            &executions,
        )
        .await
        .expect("ingest succeeds");

        assert_eq!(result.model_calls_upserted, 1);

        let (execution_cost,): (Option<i64>,) = sqlx::query_as(
            "SELECT estimated_cost_micro_usd FROM executions WHERE trace_id = $1 AND span_id = $2",
        )
        .bind("trace-no-pricing")
        .bind("span-no-pricing")
        .fetch_one(&pool)
        .await
        .expect("execution row exists");
        assert_eq!(
            execution_cost, None,
            "no pricing row means unknown cost, never a zero default"
        );
    }

    /// Story #31 AC3: telemetry from two different providers, ingested through
    /// the one shared persistence path, is queryable together with directly
    /// comparable costs (all micro-USD).
    #[tokio::test]
    async fn two_providers_ingest_through_one_path_and_query_together() {
        let Some(pool) = connected_pool().await else {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        };
        let (tenant_id, claude_integration, codex_integration) =
            fixture_two_providers(&pool, "claude_code", "codex").await;

        // Same per-token price for both models so the comparison is direct.
        async fn insert_price(pool: &PgPool, model: &str, rate: i64) {
            sqlx::query(
                "INSERT INTO model_pricing (id, model, input_per_million_micro_usd, \
                 output_per_million_micro_usd, effective_from) \
                 VALUES ($1, $2, $3, $4, now())",
            )
            .bind(format!("price-{}", cuid::cuid2()))
            .bind(model)
            .bind(rate)
            .bind(rate)
            .execute(pool)
            .await
            .expect("insert pricing fixture");
        }
        // Unique model names so parallel tests cannot interleave competing
        // pricing rows into this test's lookup.
        const CLAUDE_MODEL: &str = "claude-probe";
        const CODEX_MODEL: &str = "gpt-probe";
        insert_price(&pool, CLAUDE_MODEL, 7_000_000).await;
        insert_price(&pool, CODEX_MODEL, 7_000_000).await;

        // claude_code execution: 10 in / 5 out = 105 micro-USD. Unique ids so
        // it cannot collide with the other DB tests' trace-1/span-1 rows.
        let mut claude = valid_execution();
        claude.trace_id = "trace-claude".to_owned();
        claude.span_id = "span-claude".to_owned();
        claude.model_calls[0].trace_id = "trace-claude".to_owned();
        claude.model_calls[0].span_id = "span-claude:mc".to_owned();
        claude.model_calls[0].model = CLAUDE_MODEL.to_owned();
        // codex execution: same token counts against gpt-probe, different ids.
        let mut codex = valid_execution();
        codex.trace_id = "trace-codex".to_owned();
        codex.span_id = "span-codex".to_owned();
        codex.model_calls[0].trace_id = "trace-codex".to_owned();
        codex.model_calls[0].span_id = "span-codex:mc".to_owned();
        codex.model_calls[0].model = CODEX_MODEL.to_owned();

        ingest_telemetry(
            &pool,
            &tenant_id,
            &claude_integration,
            "claude_code",
            std::slice::from_ref(&claude),
        )
        .await
        .expect("claude ingest succeeds");
        ingest_telemetry(
            &pool,
            &tenant_id,
            &codex_integration,
            "codex",
            std::slice::from_ref(&codex),
        )
        .await
        .expect("codex ingest succeeds");

        // One query across both providers: provider, model and cost side by
        // side, all micro-USD and therefore directly comparable.
        let rows: Vec<(String, String, Option<i64>)> = sqlx::query_as(
            "SELECT e.provider, mc.model, mc.cost_micro_usd \
             FROM model_calls mc JOIN executions e ON e.id = mc.execution_id \
             WHERE e.tenant_id = $1 ORDER BY e.provider",
        )
        .bind(&tenant_id)
        .fetch_all(&pool)
        .await
        .expect("joined query runs");

        assert_eq!(rows.len(), 2, "both providers' model calls must be stored");
        assert_eq!(rows[0].0, "claude_code");
        assert_eq!(rows[0].1, CLAUDE_MODEL);
        assert_eq!(rows[0].2, Some(105), "10*7 + 5*7, same pricing as codex");
        assert_eq!(rows[1].0, "codex");
        assert_eq!(rows[1].1, CODEX_MODEL);
        assert_eq!(
            rows[1].2, rows[0].2,
            "identical token counts at identical pricing must be equal and comparable"
        );
    }
}
