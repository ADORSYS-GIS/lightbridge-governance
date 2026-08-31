//! Tests for [`super`].
//!
//! Every fixture here is **synthetic**. The parser was validated against a
//! real spool on a developer machine, but that file carries `session.id` and
//! per-session model/tool detail, so nothing from it -- not even an excerpt --
//! is committed. The shapes below reproduce what was observed, with the
//! identifying values replaced.

use serde_json::{Value, json};

use super::{batch, record, spool};

mod checkpoint;
mod drain;
mod points;
mod transform;

/// A metrics line carrying one SUM and one HISTOGRAM, in the exact nesting
/// Copilot's file exporter writes.
pub fn metrics_line() -> String {
    json!({
        "resource": {
            "_rawAttributes": [
                ["service.name", "copilot-chat"],
                ["service.version", "0.62.0"],
                ["session.id", "00000000-0000-0000-0000-000000000000"],
            ],
            "_asyncAttributesPending": false,
        },
        "scopeMetrics": [{
            "scope": { "name": "copilot-chat", "version": "0.62.0" },
            "metrics": [
                {
                    "descriptor": {
                        "name": "copilot_chat.session.count",
                        "type": "COUNTER",
                        "description": "",
                        "unit": "",
                        "valueType": 1,
                        "advice": {},
                    },
                    "aggregationTemporality": 1,
                    "dataPointType": 3,
                    "dataPoints": [{
                        "attributes": {},
                        "startTime": [1788191912, 133000000],
                        "endTime": [1788191916, 86000000],
                        "value": 1,
                    }],
                    "isMonotonic": true,
                },
                {
                    "descriptor": {
                        "name": "gen_ai.client.token.usage",
                        "type": "HISTOGRAM",
                        "description": "",
                        "unit": "",
                        "valueType": 1,
                        "advice": {},
                    },
                    "aggregationTemporality": 1,
                    "dataPointType": 0,
                    "dataPoints": [{
                        "attributes": {
                            "gen_ai.token.type": "input",
                            "gen_ai.request.model": "governed-sonnet",
                        },
                        "startTime": [1788191912, 751000000],
                        "endTime": [1788191916, 86000000],
                        "value": {
                            "min": 253,
                            "max": 258,
                            "sum": 764,
                            "buckets": { "boundaries": [1, 4], "counts": [0, 1, 2] },
                            "count": 3,
                        },
                    }],
                },
            ],
        }],
    })
    .to_string()
}

/// A log line, with the same private `_`-prefixed fields the SDK emits.
pub fn log_line() -> String {
    json!({
        "hrTime": [1788191912, 133000000],
        "hrTimeObserved": [1788191912, 133000000],
        "spanContext": {
            "traceId": "0123456789abcdef0123456789abcdef",
            "spanId": "0123456789abcdef",
            "traceFlags": 1,
        },
        "instrumentationScope": { "name": "copilot-chat", "version": "0.62.0" },
        "resource": {
            "_rawAttributes": [["service.name", "copilot-chat"]],
            "_asyncAttributesPending": false,
        },
        "attributes": {
            "event.name": "copilot_chat.tool.call",
            "gen_ai.tool.name": "manage_todo_list",
            "success": true,
            "duration_ms": 123,
        },
        "_body": "copilot_chat.tool.call: manage_todo_list",
        "totalAttributesCount": 4,
        "_isReadonly": true,
        "_logRecordLimits": { "attributeCountLimit": 128, "attributeValueLengthLimit": null },
    })
    .to_string()
}

/// A metrics line whose only metric uses `dataPointType: 1`
/// (`EXPONENTIAL_HISTOGRAM`), which this build deliberately does not
/// translate.
pub fn unknown_data_point_type_line() -> String {
    json!({
        "resource": { "_rawAttributes": [["service.name", "copilot-chat"]] },
        "scopeMetrics": [{
            "scope": { "name": "copilot-chat" },
            "metrics": [{
                "descriptor": { "name": "some.exponential", "valueType": 1 },
                "aggregationTemporality": 1,
                "dataPointType": 1,
                "dataPoints": [{ "attributes": {}, "value": { "scale": 2 } }],
            }],
        }],
    })
    .to_string()
}

/// A metrics line carrying one metric, built to order. Lets a test vary the
/// one field it is about (a data point's `value`, a descriptor's `valueType`)
/// without restating the whole nesting Copilot writes.
pub fn metric_line(descriptor: Value, data_point_type: i64, value: Value) -> String {
    json!({
        "resource": { "_rawAttributes": [["service.name", "copilot-chat"]] },
        "scopeMetrics": [{
            "scope": { "name": "copilot-chat" },
            "metrics": [{
                "descriptor": descriptor,
                "aggregationTemporality": 1,
                "dataPointType": data_point_type,
                "dataPoints": [{ "attributes": {}, "value": value }],
                "isMonotonic": true,
            }],
        }],
    })
    .to_string()
}

fn parse(payload: &Value, pointer: &str) -> Value {
    payload.pointer(pointer).cloned().unwrap_or(Value::Null)
}
