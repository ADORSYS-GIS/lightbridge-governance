//! Data points: where a record can be *partly* wrong.
//!
//! Every function here returns `(points, dropped)` rather than just the
//! points. That shape is the whole point of the module. A `filter_map` that
//! swallows a `None` produces an export the collector accepts, a tally that
//! does not move, and a run that reports success -- so the developer's
//! evidence that a VS Code update broke the parser is a metric that quietly
//! stops arriving. Every drop below is counted by construction; there is no
//! path that discards a point without returning it in the count.
//!
//! ## The histogram invariant is enforced here, not hoped for
//!
//! OTLP requires `len(bucketCounts) == len(explicitBounds) + 1`
//! (opentelemetry-proto `metrics.proto`, `HistogramDataPoint`). Copying the JS
//! SDK's two arrays across verbatim satisfies it only as long as the SDK keeps
//! writing both; `counts` is `#[serde(default)]`, so a renamed field yields an
//! empty vector beside a populated `boundaries` and a payload no validating
//! collector will take. That matters more than "one bad export": until
//! `super::export` learned to isolate a rejected record, the same bytes
//! rebuilt the same invalid payload on every wake, forever.

use serde_json::{Map, Value, json};

use super::{
    otlp,
    record::{DataPoint, HistogramValue, Metric},
};

/// The JS SDK's `ValueType::INT`. Anything else (including absent) is treated
/// as DOUBLE, matching what the SDK actually writes -- every metric on the
/// observed spool carries `valueType: 1`, counters included.
const JS_INT: i64 = 0;

/// Common `attributes`/`startTimeUnixNano`/`timeUnixNano` for either point
/// kind.
fn base(point: &DataPoint) -> Map<String, Value> {
    let mut object = Map::new();
    object.insert(
        "attributes".to_owned(),
        Value::Array(otlp::attributes(&point.attributes)),
    );
    otlp::insert_time(&mut object, "startTimeUnixNano", point.start_time.as_ref());
    otlp::insert_time(&mut object, "timeUnixNano", point.end_time.as_ref());
    object
}

/// Sum and gauge points.
///
/// The descriptor's `valueType` selects the *preferred* encoding, not a licence
/// to change the measurement. `as_i64` rejects any JSON float, so `1.0` under
/// `valueType: 0` used to be dropped outright; an integral float is emitted as
/// `asInt` instead, and a genuinely fractional one as `asDouble`, because OTLP
/// lets a `NumberDataPoint` carry either and truncating a measurement to match
/// a hint would be worse than disagreeing with the hint. Only a `value` that
/// is not a number at all is dropped -- and counted.
pub fn number(metric: &Metric) -> (Vec<Value>, usize) {
    let prefer_int = metric.descriptor.value_type == Some(JS_INT);
    let mut points = Vec::new();
    let mut dropped: usize = 0;

    for point in &metric.data_points {
        let Some(number) = point.value.as_f64() else {
            dropped = dropped.saturating_add(1);
            continue;
        };
        let mut object = base(point);
        match integral(prefer_int, &point.value, number) {
            // `asInt` is a proto3 int64, so it travels as a string.
            Some(integer) => {
                object.insert("asInt".to_owned(), Value::String(integer.to_string()));
            }
            None => {
                object.insert("asDouble".to_owned(), json!(number));
            }
        }
        points.push(Value::Object(object));
    }

    (points, dropped)
}

/// Largest integer an `f64` represents exactly (2^53). Past it, "is this a
/// whole number?" stops being a question the float can answer, so the value
/// travels as `asDouble` -- imprecise and honest about it, rather than a
/// saturating `as i64` that would invent a number.
const EXACT_INTEGER_LIMIT: f64 = 9_007_199_254_740_992.0;

/// The `asInt` value to use, or `None` to send a double. `as_i64` first, so an
/// integer JSON spelled without a decimal point never round-trips through the
/// float at all.
fn integral(prefer_int: bool, value: &Value, number: f64) -> Option<i64> {
    if !prefer_int {
        return None;
    }
    value.as_i64().or_else(|| {
        (number.fract() == 0.0 && number.abs() <= EXACT_INTEGER_LIMIT).then_some(number as i64)
    })
}

/// Histogram points, dropping (and counting) any whose value is not a
/// histogram at all or whose buckets break OTLP's invariant.
pub fn histogram(metric: &Metric) -> (Vec<Value>, usize) {
    let mut points = Vec::new();
    let mut dropped: usize = 0;

    for point in &metric.data_points {
        let parsed: Result<HistogramValue, _> = serde_json::from_value(point.value.clone());
        let Ok(value) = parsed else {
            dropped = dropped.saturating_add(1);
            continue;
        };
        // The invariant, checked before anything is built: a payload that
        // breaks it is refused by a validating collector, and refusing it here
        // costs one point instead of the batch it would have travelled in.
        if value.buckets.counts.len() != value.buckets.boundaries.len().saturating_add(1) {
            dropped = dropped.saturating_add(1);
            continue;
        }

        let mut object = base(point);
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
        points.push(Value::Object(object));
    }

    (points, dropped)
}
