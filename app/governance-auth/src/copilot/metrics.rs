//! Metrics lines -> OTLP `ResourceMetrics`.
//!
//! The whole job is translating two enums the JS SDK numbers differently from
//! the wire format, and one that it numbers the same way by coincidence:
//!
//! | JS SDK                                    | OTLP proto                       |
//! |-------------------------------------------|----------------------------------|
//! | `DataPointType` 0=HIST 1=EXP 2=GAUGE 3=SUM | chooses the `Metric.data` oneof  |
//! | `AggregationTemporality` 0=DELTA 1=CUM     | 1=DELTA 2=CUM (0 = unspecified)  |
//! | `ValueType` 0=INT 1=DOUBLE                 | chooses `asInt` vs `asDouble`    |
//!
//! Getting temporality wrong is not cosmetic: a collector told CUMULATIVE
//! about DELTA data computes rates from the wrong baseline, so the numbers
//! stay plausible while being wrong. That is why the mapping is a named
//! function with its own test rather than an inline `+ 1`.
//!
//! `EXPONENTIAL_HISTOGRAM` is deliberately **not** translated. It needs a
//! different data point shape (scale/zero-count/positive-negative buckets)
//! than anything Copilot has been observed to emit, and a guessed mapping
//! would ship wrong numbers rather than no numbers. It is skipped and
//! counted, like any other unknown type.

use serde_json::{Map, Value, json};

use super::{
    otlp,
    record::{DataPoint, HistogramValue, Metric, MetricsRecord},
};

const JS_HISTOGRAM: i64 = 0;
const JS_GAUGE: i64 = 2;
const JS_SUM: i64 = 3;

const JS_DELTA: i64 = 0;
const JS_CUMULATIVE: i64 = 1;

const OTLP_UNSPECIFIED: i64 = 0;
const OTLP_DELTA: i64 = 1;
const OTLP_CUMULATIVE: i64 = 2;

/// The JS SDK's `ValueType::INT`. Anything else (including absent) is treated
/// as DOUBLE, matching what the SDK actually writes -- every metric on the
/// observed spool carries `valueType: 1`, counters included.
const JS_INT: i64 = 0;

/// Transforms one metrics line. Returns the `(resource, scope, metrics)`
/// triples for [`otlp::group`] and the number of metrics skipped because
/// their `dataPointType` is one this build does not translate.
pub fn transform(record: &MetricsRecord) -> (Vec<otlp::Grouped>, usize) {
    let resource = otlp::resource(&record.resource);
    let mut skipped: usize = 0;
    let mut groups = Vec::new();

    for scope_metrics in &record.scope_metrics {
        let mut metrics = Vec::new();
        for source in &scope_metrics.metrics {
            match metric(source) {
                Some(value) => metrics.push(value),
                None => skipped = skipped.saturating_add(1),
            }
        }
        groups.push((resource.clone(), otlp::scope(&scope_metrics.scope), metrics));
    }

    (groups, skipped)
}

/// One `Metric`, or `None` when its `dataPointType` is absent or unsupported.
fn metric(metric: &Metric) -> Option<Value> {
    let (field, data) = match metric.data_point_type? {
        JS_HISTOGRAM => (
            "histogram",
            json!({
                "dataPoints": histogram_points(metric),
                "aggregationTemporality": temporality(metric.aggregation_temporality),
            }),
        ),
        JS_GAUGE => ("gauge", json!({ "dataPoints": number_points(metric) })),
        JS_SUM => (
            "sum",
            json!({
                "dataPoints": number_points(metric),
                "aggregationTemporality": temporality(metric.aggregation_temporality),
                "isMonotonic": metric.is_monotonic.unwrap_or(false),
            }),
        ),
        _ => return None,
    };

    let mut object = Map::new();
    object.insert(
        "name".to_owned(),
        Value::String(metric.descriptor.name.clone()),
    );
    object.insert(
        "description".to_owned(),
        Value::String(metric.descriptor.description.clone()),
    );
    object.insert(
        "unit".to_owned(),
        Value::String(metric.descriptor.unit.clone()),
    );
    object.insert(field.to_owned(), data);
    Some(Value::Object(object))
}

/// JS `AggregationTemporality` -> OTLP's. An unrecognised or absent value
/// maps to `UNSPECIFIED` rather than guessing CUMULATIVE: a collector that
/// sees UNSPECIFIED can reject or default explicitly, whereas a wrong-but-
/// valid temporality is accepted and silently miscomputed.
fn temporality(js: Option<i64>) -> i64 {
    match js {
        Some(JS_DELTA) => OTLP_DELTA,
        Some(JS_CUMULATIVE) => OTLP_CUMULATIVE,
        _ => OTLP_UNSPECIFIED,
    }
}

/// Common `attributes`/`startTimeUnixNano`/`timeUnixNano` for either point
/// kind.
fn base_point(point: &DataPoint) -> Map<String, Value> {
    let mut object = Map::new();
    object.insert(
        "attributes".to_owned(),
        Value::Array(otlp::attributes(&point.attributes)),
    );
    otlp::insert_time(&mut object, "startTimeUnixNano", point.start_time.as_ref());
    otlp::insert_time(&mut object, "timeUnixNano", point.end_time.as_ref());
    object
}

fn number_points(metric: &Metric) -> Vec<Value> {
    let as_int = metric.descriptor.value_type == Some(JS_INT);
    metric
        .data_points
        .iter()
        .filter_map(|point| {
            let mut object = base_point(point);
            if as_int {
                // `asInt` is a proto3 int64, so it travels as a string.
                let integer = point.value.as_i64()?;
                object.insert("asInt".to_owned(), Value::String(integer.to_string()));
            } else {
                object.insert("asDouble".to_owned(), json!(point.value.as_f64()?));
            }
            Some(Value::Object(object))
        })
        .collect()
}

fn histogram_points(metric: &Metric) -> Vec<Value> {
    metric
        .data_points
        .iter()
        .filter_map(|point| {
            let value: HistogramValue = serde_json::from_value(point.value.clone()).ok()?;
            let mut object = base_point(point);
            object.insert(
                "count".to_owned(),
                Value::String(value.count.unwrap_or_default().to_string()),
            );
            if let Some(sum) = value.sum {
                object.insert("sum".to_owned(), json!(sum));
            }
            if let Some(min) = value.min {
                object.insert("min".to_owned(), json!(min));
            }
            if let Some(max) = value.max {
                object.insert("max".to_owned(), json!(max));
            }
            let counts: Vec<Value> = value
                .buckets
                .counts
                .iter()
                .map(|count| Value::String(count.to_string()))
                .collect();
            object.insert("bucketCounts".to_owned(), Value::Array(counts));
            object.insert("explicitBounds".to_owned(), json!(value.buckets.boundaries));
            Some(Value::Object(object))
        })
        .collect()
}
