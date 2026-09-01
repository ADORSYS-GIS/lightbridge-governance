//! The JS-SDK -> OTLP transform, against synthetic fixtures.

use super::*;

#[test]
fn a_metrics_line_becomes_otlp_resource_metrics() {
    let built = batch::build(&[metrics_line()]);
    let payload = built.metrics.unwrap_or(Value::Null);

    assert_eq!(built.counts.metrics, 1, "{}", built.counts.describe());
    assert!(built.logs.is_none(), "a metrics line must not produce logs");

    assert_eq!(
        parse(&payload, "/resourceMetrics/0/resource/attributes/0"),
        json!({ "key": "service.name", "value": { "stringValue": "copilot-chat" } }),
        "_rawAttributes pairs must become OTLP KeyValues"
    );
    assert_eq!(
        parse(&payload, "/resourceMetrics/0/scopeMetrics/0/scope"),
        json!({ "name": "copilot-chat", "version": "0.62.0" })
    );

    let sum = parse(&payload, "/resourceMetrics/0/scopeMetrics/0/metrics/0");
    assert_eq!(parse(&sum, "/name"), json!("copilot_chat.session.count"));
    assert_eq!(parse(&sum, "/sum/isMonotonic"), json!(true));
    // JS CUMULATIVE (1) is OTLP CUMULATIVE (2), not 1. Getting this wrong
    // leaves the numbers plausible and the rates wrong.
    assert_eq!(parse(&sum, "/sum/aggregationTemporality"), json!(2));
    assert_eq!(parse(&sum, "/sum/dataPoints/0/asDouble"), json!(1.0));
    // [seconds, nanos] -> a proto3 uint64, i.e. a STRING.
    assert_eq!(
        parse(&sum, "/sum/dataPoints/0/startTimeUnixNano"),
        json!("1788191912133000000")
    );
    assert_eq!(
        parse(&sum, "/sum/dataPoints/0/timeUnixNano"),
        json!("1788191916086000000")
    );

    let histogram = parse(&payload, "/resourceMetrics/0/scopeMetrics/0/metrics/1");
    assert_eq!(
        parse(&histogram, "/name"),
        json!("gen_ai.client.token.usage")
    );
    assert_eq!(
        parse(&histogram, "/histogram/dataPoints/0/count"),
        json!("3")
    );
    assert_eq!(
        parse(&histogram, "/histogram/dataPoints/0/sum"),
        json!(764.0)
    );
    assert_eq!(
        parse(&histogram, "/histogram/dataPoints/0/bucketCounts"),
        json!(["0", "1", "2"])
    );
    assert_eq!(
        parse(&histogram, "/histogram/dataPoints/0/explicitBounds"),
        json!([1.0, 4.0])
    );
    assert_eq!(
        parse(&histogram, "/histogram/dataPoints/0/attributes/0"),
        json!({ "key": "gen_ai.token.type", "value": { "stringValue": "input" } })
    );
}

#[test]
fn a_log_line_becomes_otlp_resource_logs() {
    let built = batch::build(&[log_line()]);
    let payload = built.logs.unwrap_or(Value::Null);

    assert_eq!(built.counts.logs, 1, "{}", built.counts.describe());
    assert!(
        built.metrics.is_none(),
        "a log line must not produce metrics"
    );

    let record = parse(&payload, "/resourceLogs/0/scopeLogs/0/logRecords/0");
    assert_eq!(
        parse(&record, "/timeUnixNano"),
        json!("1788191912133000000")
    );
    assert_eq!(
        parse(&record, "/observedTimeUnixNano"),
        json!("1788191912133000000")
    );
    assert_eq!(
        parse(&record, "/body"),
        json!({ "stringValue": "copilot_chat.tool.call: manage_todo_list" })
    );
    assert_eq!(
        parse(&record, "/traceId"),
        json!("0123456789abcdef0123456789abcdef")
    );
    assert_eq!(parse(&record, "/spanId"), json!("0123456789abcdef"));
    assert_eq!(parse(&record, "/flags"), json!(1));
    // The three attribute value kinds that actually appear, each with the
    // right AnyValue field -- an int written as `doubleValue` would be
    // accepted by the collector and quietly change the type of the series.
    assert_eq!(
        parse(&record, "/attributes/2"),
        json!({ "key": "success", "value": { "boolValue": true } })
    );
    assert_eq!(
        parse(&record, "/attributes/3"),
        json!({ "key": "duration_ms", "value": { "intValue": "123" } })
    );
    // No severity is synthesised: Copilot writes none. See `logs`'s module doc.
    assert_eq!(parse(&record, "/severityNumber"), Value::Null);
}

#[test]
fn a_malformed_line_is_counted_and_skipped_not_fatal() {
    // Blank lines are absent on purpose: `spool::drain` strips them before
    // this ever sees them, so counting one here would test the fixture.
    let built = batch::build(&[
        "{not json at all".to_owned(),
        metrics_line(),
        "{}".to_owned(),
        log_line(),
    ]);

    assert_eq!(built.counts.unparsable, 1, "{}", built.counts.describe());
    // The literal `{}` records Copilot really writes: JSON, neither shape, and
    // carrying nothing -- counted apart from loss so a healthy spool's 22-in-98
    // of them do not drown the counter that matters.
    assert_eq!(built.counts.empty, 1, "{}", built.counts.describe());
    assert_eq!(built.counts.unknown, 0, "{}", built.counts.describe());
    assert_eq!(
        built.counts.discarded(),
        1,
        "only the unparsable line is loss: {}",
        built.counts.describe()
    );
    assert_eq!(built.counts.metrics, 1);
    assert_eq!(built.counts.logs, 1);
    assert!(
        built.metrics.is_some() && built.logs.is_some(),
        "one bad line must not cost the good ones"
    );
}

#[test]
fn an_unknown_data_point_type_is_counted_and_skipped() {
    let built = batch::build(&[unknown_data_point_type_line()]);

    assert_eq!(
        built.counts.unsupported_metrics,
        1,
        "{}",
        built.counts.describe()
    );
    assert_eq!(built.counts.metrics, 0, "nothing translatable was found");
    assert!(
        built.metrics.is_none(),
        "an empty export must not be posted at all"
    );
}

#[test]
fn records_sharing_a_resource_and_scope_are_grouped_into_one_export() {
    let built = batch::build(&[log_line(), log_line(), log_line()]);
    let payload = built.logs.unwrap_or(Value::Null);

    assert_eq!(
        parse(&payload, "/resourceLogs")
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        1,
        "the resource is a session constant; repeating it per record is waste"
    );
    assert_eq!(
        parse(&payload, "/resourceLogs/0/scopeLogs/0/logRecords")
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        3
    );
}

#[test]
fn classify_does_not_mistake_an_empty_object_for_metrics() {
    // 22 of the 98 records on the observed spool are a literal `{}`. A
    // `#[serde(untagged)]` enum would classify every one of them as metrics,
    // because both record types deserialise from `{}` successfully.
    //
    // `Empty`, not `Unknown`: they carry nothing, so they are not loss. See
    // `record::classify` for why that distinction is load-bearing.
    assert_eq!(record::classify(&json!({})), record::Kind::Empty);
    assert_eq!(record::classify(&json!([])), record::Kind::Unknown);
    assert_eq!(
        record::classify(&json!({ "scopeMetrics": [] })),
        record::Kind::Metrics
    );
    assert_eq!(
        record::classify(&json!({ "_body": "x" })),
        record::Kind::Log
    );
}
