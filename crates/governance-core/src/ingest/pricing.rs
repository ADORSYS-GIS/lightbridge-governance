//! Model-call pricing: the `model_pricing` lookup and the overflow-safe cost
//! computation.

use cratestack::{cratestack_error_from_sqlx, sqlx};
use sqlx::{Postgres, Transaction};

use super::types::ModelCallInput;
use crate::{Error, MicroUsd, Result};

/// Calculates the cost of a model call from the pricing table.
///
/// Uses the most recent pricing entry for the model that is effective at the
/// time of the call. Returns `None` -- cost *unknown* -- when the call has no
/// token counts, or when no pricing exists for the model. Unknown is honest:
/// a zero would be indistinguishable from "free" on a dashboard (story #31
/// AC6). The call is still ingested, just unpriced.
pub(crate) async fn calculate_model_cost(
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
    .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;

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
