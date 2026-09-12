use serde_json::json;

use super::*;

fn request() -> ExportLogsServiceRequest {
    serde_json::from_value(json!({"resourceLogs":[{"resource":{"attributes":[
    {"key":"service.name","value":{"stringValue":"codex-app-server"}}]},
    "scopeLogs":[{"logRecords":[{"timeUnixNano":"1234567890","attributes":[
        {"key":"event.name","value":{"stringValue":"codex.sse_event"}},
        {"key":"event.kind","value":{"stringValue":"response.completed"}},
        {"key":"model","value":{"stringValue":"gpt-6-astra"}},
        {"key":"input_token_count","value":{"intValue":"100"}},
        {"key":"cached_token_count","value":{"intValue":"80"}},
        {"key":"cache_write_token_count","value":{"intValue":"0"}},
        {"key":"output_token_count","value":{"intValue":"10"}}
    ]}]}]}]}))
    .unwrap()
}

fn first(request: &ExportLogsServiceRequest) -> &LogRecord {
    &request.resource_logs[0].scope_logs[0].log_records[0]
}

#[test]
fn cached_input_is_not_charged_twice_and_long_context_uses_full_request_rates() {
    assert_eq!(price(100, 80, 0, 10), Some(780));
    assert_eq!(price(272_000, 0, 0, 10), Some(2_720_500));
    assert_eq!(price(272_001, 0, 0, 10), Some(5_440_770));
}

#[test]
fn unsupported_cache_writes_invalid_counters_and_overflow_are_unknown() {
    assert_eq!(price(100, 80, 1, 10), None);
    assert_eq!(price(100, 101, 0, 10), None);
    assert_eq!(price(u64::MAX, 0, 0, u64::MAX), None);
    assert_eq!(price(0, 0, 0, 0), Some(0));
}

#[test]
fn protobuf_and_json_produce_identical_estimates_and_reenrichment_is_idempotent() {
    let request = request();
    let binary = enrich(&request.encode_to_vec(), WireFormat::Protobuf);
    let binary_decoded = ExportLogsServiceRequest::decode(binary.as_slice()).unwrap();
    let json = enrich(&serde_json::to_vec(&request).unwrap(), WireFormat::Json);
    let json_decoded: ExportLogsServiceRequest = serde_json::from_slice(&json).unwrap();
    assert_eq!(binary_decoded, json_decoded);
    assert_eq!(
        integer(first(&json_decoded), "governance.estimate.micro_usd"),
        Some(780)
    );
    assert_eq!(enrich(&binary, WireFormat::Protobuf), binary);
    assert_eq!(enrich(&json, WireFormat::Json), json);
}

#[test]
fn missing_counters_and_unknown_models_do_not_become_zero_cost() {
    let mut request = request();
    request.resource_logs[0].scope_logs[0].log_records[0]
        .attributes
        .retain(|a| a.key != "cached_token_count");
    assert_eq!(estimate(first(&request)), None);
    let mut record = first(&self::request()).clone();
    record.attributes.retain(|a| a.key != "model");
    record.attributes.push(KeyValue {
        key: "model".into(),
        value: Some(AnyValue {
            value: Some(Value::StringValue("gpt-6-astra-future".into())),
        }),
        ..Default::default()
    });
    assert_eq!(estimate(&record), None);
}

#[test]
fn estimates_never_write_actual_billing_fields_or_price_other_event_types() {
    let input = serde_json::to_vec(&request()).unwrap();
    let output = String::from_utf8(enrich(&input, WireFormat::Json)).unwrap();
    assert!(!output.contains("actual"));
    let other = String::from_utf8(input)
        .unwrap()
        .replace("response.completed", "response.started");
    assert_eq!(enrich(other.as_bytes(), WireFormat::Json), other.as_bytes());
}
