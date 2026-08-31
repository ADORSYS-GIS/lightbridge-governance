//! Log lines -> OTLP `ResourceLogs`.
//!
//! Copilot's log records are event-shaped: a short string body
//! (`copilot_chat.tool.call: manage_todo_list`) plus the attributes that
//! actually carry the governance signal (`gen_ai.tool.name`, `success`,
//! `duration_ms`, `gen_ai.request.model`).
//!
//! Two fields are deliberately **not** synthesised:
//!
//! - **`severityNumber`/`severityText`.** The exporter writes neither, and a
//!   default of INFO would be this drain asserting something Copilot never
//!   said. OTLP treats an absent severity as unspecified, which is the true
//!   answer.
//! - **`droppedAttributesCount`.** `totalAttributesCount` is the SDK's count
//!   *before* limits, and subtracting the two would be a guess about which
//!   side of `_logRecordLimits` a record fell on.

use serde_json::{Map, Value};

use super::{otlp, record::LogRecord};

/// Transforms one log line into a single `(resource, scope, [logRecord])`
/// triple for [`otlp::group`], which is where the per-line repetition of the
/// (session-constant) resource and scope is collapsed.
pub fn transform(record: &LogRecord) -> otlp::Grouped {
    let mut object = Map::new();
    otlp::insert_time(&mut object, "timeUnixNano", record.hr_time.as_ref());
    otlp::insert_time(
        &mut object,
        "observedTimeUnixNano",
        record.hr_time_observed.as_ref(),
    );
    if let Some(body) = &record.body {
        object.insert("body".to_owned(), otlp::any_value(body));
    }
    object.insert(
        "attributes".to_owned(),
        Value::Array(otlp::attributes(&record.attributes)),
    );

    // Trace correlation travels as lowercase hex in OTLP/JSON, which is
    // exactly how the JS SDK already holds it -- so this is a copy, not a
    // re-encoding, and an unset/invalid id is omitted rather than zero-filled.
    if let Some(span) = &record.span_context {
        if let Some(trace_id) = &span.trace_id {
            object.insert("traceId".to_owned(), Value::String(trace_id.clone()));
        }
        if let Some(span_id) = &span.span_id {
            object.insert("spanId".to_owned(), Value::String(span_id.clone()));
        }
        if let Some(flags) = span.trace_flags {
            object.insert("flags".to_owned(), Value::Number(flags.into()));
        }
    }

    (
        otlp::resource(&record.resource),
        otlp::scope(&record.instrumentation_scope),
        vec![Value::Object(object)],
    )
}
