//! Estimate at admission so a retry cannot silently apply a different rate card.
//! Native tokens remain authoritative. These annotations are never actual cost.
use opentelemetry_proto::tonic::{
    collector::logs::v1::ExportLogsServiceRequest,
    common::v1::{AnyValue, KeyValue, any_value::Value},
    logs::v1::LogRecord,
};
use prost::Message;

use super::receive::WireFormat;

const PREFIX: &str = "governance.estimate.";
const RATE_CARD: &str = "openai-standard-us-2026-09-12";

pub(super) fn enrich(body: &[u8], format: WireFormat) -> Vec<u8> {
    let parsed = match format {
        WireFormat::Json => serde_json::from_slice(body).ok(),
        WireFormat::Protobuf => ExportLogsServiceRequest::decode(body).ok(),
    };
    let Some(mut request) = parsed else {
        return body.to_vec();
    };
    let mut changed = false;
    for record in request
        .resource_logs
        .iter_mut()
        .flat_map(|r| &mut r.scope_logs)
        .flat_map(|s| &mut s.log_records)
    {
        if string(record, "event.name") != Some("codex.sse_event")
            || string(record, "event.kind") != Some("response.completed")
        {
            continue;
        }
        changed = true;
        record.attributes.retain(|a| !a.key.starts_with(PREFIX));
        let estimate = estimate(record);
        attr(
            record,
            "status",
            Value::StringValue(
                if estimate.is_some() {
                    "priced"
                } else {
                    "unpriced"
                }
                .into(),
            ),
        );
        attr(record, "rate_card", Value::StringValue(RATE_CARD.into()));
        if let Some(cost) = estimate {
            attr(record, "micro_usd", Value::IntValue(cost));
        }
    }
    if !changed {
        return body.to_vec();
    }
    match format {
        WireFormat::Json => serde_json::to_vec(&request).unwrap_or_else(|_| body.to_vec()),
        WireFormat::Protobuf => request.encode_to_vec(),
    }
}

fn string<'a>(record: &'a LogRecord, key: &str) -> Option<&'a str> {
    let mut matches = record.attributes.iter().filter(|a| a.key == key);
    let value = matches.next()?.value.as_ref()?.value.as_ref()?;
    if matches.next().is_some() {
        return None;
    }
    if let Value::StringValue(v) = value {
        Some(v)
    } else {
        None
    }
}

fn integer(record: &LogRecord, key: &str) -> Option<u64> {
    let mut matches = record.attributes.iter().filter(|a| a.key == key);
    let value = matches.next()?.value.as_ref()?.value.as_ref()?;
    if matches.next().is_some() {
        return None;
    }
    match value {
        Value::IntValue(v) => u64::try_from(*v).ok(),
        Value::StringValue(v) => v.parse().ok(),
        _ => None,
    }
}

fn attr(record: &mut LogRecord, suffix: &str, value: Value) {
    record.attributes.push(KeyValue {
        key: format!("{PREFIX}{suffix}"),
        value: Some(AnyValue { value: Some(value) }),
        ..Default::default()
    });
}

fn estimate(record: &LogRecord) -> Option<i64> {
    // Only Astra's complete token and long-context tariff has been verified.
    // Do not guess rates for aliases, internal reviewers or a future model.
    if string(record, "model")? != "gpt-6-astra" {
        return None;
    }
    // A standard US API-equivalent baseline, not a claim about subscription
    // debits, service-tier discounts/uplifts, tool fees, or the user's invoice.
    let input = integer(record, "input_token_count")?;
    let cached = integer(record, "cached_token_count")?;
    let writes = integer(record, "cache_write_token_count")?;
    let output = integer(record, "output_token_count")?;
    price(input, cached, writes, output)
}

fn price(input: u64, cached: u64, writes: u64, output: u64) -> Option<i64> {
    // Cache writes' inclusion in Codex input has not been verified. Refuse a
    // nonzero write count until it is; silently adding or subtracting it risks
    // double billing. Zero is an observed value, never a missing-field default.
    if writes != 0 {
        return None;
    }
    let uncached = input.checked_sub(cached)?;
    let (input_rate, cache_rate, output_rate) = if input > 272_000 {
        (20_u128, 2_u128, 75_u128)
    } else {
        (10_u128, 1_u128, 50_u128)
    };
    // USD per million tokens equals micro-USD per token for these integral
    // rates. No floats, intermediate i64 overflow, or per-category rounding.
    i64::try_from(
        u128::from(uncached) * input_rate
            + u128::from(cached) * cache_rate
            + u128::from(output) * output_rate,
    )
    .ok()
}

#[cfg(test)]
mod tests;
