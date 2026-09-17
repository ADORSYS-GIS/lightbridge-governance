use chrono::Utc;

use super::*;
use crate::MicroUsd;

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
        identity_mismatch_detection_failed: false,
        identity_mismatches: 0,
    };

    let json = serde_json::to_string(&result).expect("serialize");
    assert!(json.contains("\"executions_upserted\":5"));
    assert!(json.contains("\"model_calls_upserted\":10"));
    assert!(json.contains("\"tool_calls_upserted\":3"));
    assert!(json.contains("\"identity_mismatch_detection_failed\":false"));
    assert!(json.contains("\"identity_mismatches\":0"));
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
    let mut attempt = 0;
    loop {
        match fixture_inner(pool, provider).await {
            Ok(result) => return result,
            Err(e) if is_deadlock(&e) && attempt < MAX_DEADLOCK_RETRIES => {
                attempt += 1;
                let delay = DEADLOCK_RETRY_BASE_MS * 2u64.pow(attempt - 1);
                tracing::warn!(
                    attempt,
                    delay_ms = delay,
                    "deadlock detected in fixture setup, retrying"
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            Err(e) => panic!("fixture setup failed: {e:?}"),
        }
    }
}

async fn fixture_inner(
    pool: &PgPool,
    provider: &str,
) -> std::result::Result<(String, String), cratestack_core::CratestackError> {
    let tenant_id = format!("tenant-{}", cuid::cuid2());
    let application_id = format!("app-{}", cuid::cuid2());
    let environment_id = format!("env-{}", cuid::cuid2());
    let integration_id = format!("integration-{}", cuid::cuid2());
    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, $2)")
        .bind(&tenant_id)
        .bind("ingest-test-tenant")
        .execute(pool)
        .await
        .map_err(cratestack_error_from_sqlx)?;
    sqlx::query("INSERT INTO applications (id, tenant_id, name) VALUES ($1, $2, $3)")
        .bind(&application_id)
        .bind(&tenant_id)
        .bind("ingest-test-app")
        .execute(pool)
        .await
        .map_err(cratestack_error_from_sqlx)?;
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
    .map_err(cratestack_error_from_sqlx)?;
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
    .map_err(cratestack_error_from_sqlx)?;
    Ok((tenant_id, integration_id))
}

/// Like `fixture`, but creates two integrations under the same tenant.
/// Used by the two-providers test to verify cross-provider queries.
async fn fixture_two_providers(
    pool: &PgPool,
    provider_a: &str,
    provider_b: &str,
) -> (String, String, String) {
    let mut attempt = 0;
    loop {
        match fixture_two_providers_inner(pool, provider_a, provider_b).await {
            Ok(result) => return result,
            Err(e) if is_deadlock(&e) && attempt < MAX_DEADLOCK_RETRIES => {
                attempt += 1;
                let delay = DEADLOCK_RETRY_BASE_MS * 2u64.pow(attempt - 1);
                tracing::warn!(
                    attempt,
                    delay_ms = delay,
                    "deadlock detected in fixture_two_providers setup, retrying"
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            Err(e) => panic!("fixture_two_providers setup failed: {e:?}"),
        }
    }
}

async fn fixture_two_providers_inner(
    pool: &PgPool,
    provider_a: &str,
    provider_b: &str,
) -> std::result::Result<(String, String, String), cratestack_core::CratestackError> {
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
        .map_err(cratestack_error_from_sqlx)?;
    sqlx::query("INSERT INTO applications (id, tenant_id, name) VALUES ($1, $2, $3)")
        .bind(&application_id)
        .bind(&tenant_id)
        .bind("ingest-test-app")
        .execute(pool)
        .await
        .map_err(cratestack_error_from_sqlx)?;
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
    .map_err(cratestack_error_from_sqlx)?;
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
    .map_err(cratestack_error_from_sqlx)?;
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
    .map_err(cratestack_error_from_sqlx)?;
    Ok((tenant_id, integration_a, integration_b))
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
    // Unique ids: valid_execution() defaults to trace-1/span-1, which other DB
    // tests also use -- parallel tests must not collide on rows (the
    // (trace_id, span_id) unique index is global, not tenant-scoped).
    execution.trace_id = "trace-idem".to_owned();
    execution.span_id = "span-idem".to_owned();
    execution.model_calls[0].trace_id = "trace-idem".to_owned();
    execution.model_calls[0].span_id = "span-idem:mc".to_owned();
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
    .bind("trace-idem")
    .bind("span-idem")
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
    // Unique ids: valid_execution() defaults to trace-1/span-1, which other DB
    // tests also use -- parallel tests must not collide on rows.
    execution.trace_id = "trace-reprice".to_owned();
    execution.span_id = "span-reprice".to_owned();
    execution.model_calls[0].trace_id = "trace-reprice".to_owned();
    execution.model_calls[0].span_id = "span-reprice:mc".to_owned();
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
    .bind("trace-reprice")
    .bind("span-reprice")
    .fetch_one(&pool)
    .await
    .expect("execution row exists");
    assert_eq!(
        execution_cost,
        Some(105),
        "a pricing change must not rewrite already-stored costs"
    );
}

/// The execution total must saturate, never wrap: per-call costs are clamped
/// to `i64::MAX` in `pricing.rs` (token counts are attacker-controlled), so
/// two clamped calls summed with a plain `+=` would silently wrap negative in
/// a release build (overflow checks off). This is the failure class
/// `pricing.rs` guards against, one level up -- the accumulator must not
/// reintroduce it.
#[tokio::test]
async fn execution_total_saturates_when_costs_would_overflow() {
    let Some(pool) = connected_pool().await else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let (tenant_id, integration_id) = fixture(&pool, "claude_code").await;

    // A rate large enough that a single call's cost clamps to i64::MAX.
    const MODEL: &str = "overflow-sonnet";
    sqlx::query(
        "INSERT INTO model_pricing (id, model, input_per_million_micro_usd, \
         output_per_million_micro_usd, effective_from) \
         VALUES ($1, $2, $3, $4, now())",
    )
    .bind(format!("price-{}", cuid::cuid2()))
    .bind(MODEL)
    .bind(i64::MAX)
    .bind(i64::MAX)
    .execute(&pool)
    .await
    .expect("insert pricing fixture");

    // Two model calls, each with token counts that clamp to i64::MAX. Their
    // sum overflows i64; the accumulator must saturate at i64::MAX, not wrap.
    let mut execution = valid_execution();
    execution.trace_id = "trace-overflow".to_owned();
    execution.span_id = "span-overflow".to_owned();
    execution.model_calls = vec![
        ModelCallInput {
            trace_id: "trace-overflow".to_owned(),
            span_id: "span-overflow:mc1".to_owned(),
            model: MODEL.to_owned(),
            input_tokens: Some(i64::MAX),
            output_tokens: Some(i64::MAX),
        },
        ModelCallInput {
            trace_id: "trace-overflow".to_owned(),
            span_id: "span-overflow:mc2".to_owned(),
            model: MODEL.to_owned(),
            input_tokens: Some(i64::MAX),
            output_tokens: Some(i64::MAX),
        },
    ];
    let executions = vec![execution];

    ingest_telemetry(
        &pool,
        &tenant_id,
        &integration_id,
        "claude_code",
        &executions,
    )
    .await
    .expect("ingest succeeds");

    let (execution_cost,): (Option<i64>,) = sqlx::query_as(
        "SELECT estimated_cost_micro_usd FROM executions \
         WHERE trace_id = $1 AND span_id = $2",
    )
    .bind("trace-overflow")
    .bind("span-overflow")
    .fetch_one(&pool)
    .await
    .expect("execution row exists");

    assert_eq!(
        execution_cost,
        Some(i64::MAX),
        "two clamped costs must saturate at i64::MAX, not wrap negative"
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

    let (model_call_cost, input_tokens, output_tokens): (Option<i64>, Option<i64>, Option<i64>) =
        sqlx::query_as(
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

/// Story #33: Codex exec mode token counts are on span attributes, not metrics.
/// This test verifies that Codex-style token counts (input_tokens/output_tokens on model calls)
/// flow through the ingest pipeline and are priced correctly.
#[tokio::test]
async fn codex_style_token_counts_are_priced_correctly() {
    let Some(pool) = connected_pool().await else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let (tenant_id, integration_id) = fixture(&pool, "codex").await;

    // Unique model name so parallel tests cannot interleave a competing
    // pricing row into this test's lookup.
    const MODEL: &str = "codex-exec-probe";
    sqlx::query(
        "INSERT INTO model_pricing (id, model, input_per_million_micro_usd, \
         output_per_million_micro_usd, effective_from) \
         VALUES ($1, $2, $3, $4, now())",
    )
    .bind(format!("price-{}", cuid::cuid2()))
    .bind(MODEL)
    .bind(30_000_000i64) // $30 per million input tokens
    .bind(60_000_000i64) // $60 per million output tokens
    .execute(&pool)
    .await
    .expect("insert pricing fixture");

    // Create a Codex exec mode execution with token counts
    let mut execution = valid_execution();
    execution.trace_id = "trace-codex-exec".to_owned();
    execution.span_id = "span-codex-exec".to_owned();
    execution.model_calls[0].trace_id = "trace-codex-exec".to_owned();
    execution.model_calls[0].span_id = "span-codex-exec:mc".to_owned();
    execution.model_calls[0].model = MODEL.to_owned();
    execution.model_calls[0].input_tokens = Some(1000);
    execution.model_calls[0].output_tokens = Some(500);

    let executions = vec![execution.clone()];
    let first = ingest_telemetry(&pool, &tenant_id, &integration_id, "codex", &executions)
        .await
        .expect("codex exec ingest succeeds");

    assert_eq!(first.executions_upserted, 1);
    assert_eq!(first.model_calls_upserted, 1);

    // Verify cost calculation: 1000 * $30/M + 500 * $60/M = 30000 + 30000 = 60000 micro-USD
    let (execution_cost,): (Option<i64>,) = sqlx::query_as(
        "SELECT estimated_cost_micro_usd FROM executions WHERE trace_id = $1 AND span_id = $2",
    )
    .bind("trace-codex-exec")
    .bind("span-codex-exec")
    .fetch_one(&pool)
    .await
    .expect("execution row exists");

    assert_eq!(
        execution_cost,
        Some(60_000),
        "codex exec token counts must be priced correctly"
    );

    // Idempotency: reprocessing must not change row counts or costs.
    let second = ingest_telemetry(&pool, &tenant_id, &integration_id, "codex", &executions)
        .await
        .expect("reprocessing succeeds");

    assert_eq!(
        (second.executions_upserted, second.model_calls_upserted),
        (1, 1),
        "row counts must not change on reprocessing"
    );

    let (reprocessed_cost,): (Option<i64>,) = sqlx::query_as(
        "SELECT estimated_cost_micro_usd FROM executions WHERE trace_id = $1 AND span_id = $2",
    )
    .bind("trace-codex-exec")
    .bind("span-codex-exec")
    .fetch_one(&pool)
    .await
    .expect("execution row exists");

    assert_eq!(
        reprocessed_cost,
        Some(60_000),
        "cost must remain stable on reprocessing"
    );
}
