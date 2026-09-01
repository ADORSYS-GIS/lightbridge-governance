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
//!
//! Everything below the metric -- the data points themselves -- lives in
//! [`super::points`], because that is where a record can be *partly* wrong and
//! where the counting has to be exact.

use serde_json::{Map, Value, json};

use super::{
    otlp, points,
    record::{Metric, MetricsRecord},
};

const JS_HISTOGRAM: i64 = 0;
const JS_GAUGE: i64 = 2;
const JS_SUM: i64 = 3;

const JS_DELTA: i64 = 0;
const JS_CUMULATIVE: i64 = 1;

const OTLP_UNSPECIFIED: i64 = 0;
const OTLP_DELTA: i64 = 1;
const OTLP_CUMULATIVE: i64 = 2;

/// What one line cost, at the two levels loss can happen at. Reported
/// separately because they mean different things to whoever reads the tally:
/// `unsupported` is a metric kind this build never claimed to handle, while
/// `points` is a shape that changed under a metric kind it does.
#[derive(Debug, Default)]
pub struct Skipped {
    pub unsupported: usize,
    pub points: usize,
}

/// Transforms one metrics line into the `(resource, scope, metrics)` triples
/// [`otlp::group`] folds, plus everything the line lost on the way.
pub fn transform(record: &MetricsRecord) -> (Vec<otlp::Grouped>, Skipped) {
    let resource = otlp::resource(&record.resource);
    let mut skipped = Skipped::default();
    let mut groups = Vec::new();

    for scope_metrics in &record.scope_metrics {
        let mut metrics = Vec::new();
        for source in &scope_metrics.metrics {
            match metric(source) {
                Translated::Metric(value, dropped) => {
                    skipped.points = skipped.points.saturating_add(dropped);
                    metrics.push(value);
                }
                Translated::NoPoints(dropped) => {
                    skipped.points = skipped.points.saturating_add(dropped);
                }
                Translated::Unsupported => {
                    skipped.unsupported = skipped.unsupported.saturating_add(1);
                }
            }
        }
        groups.push((resource.clone(), otlp::scope(&scope_metrics.scope), metrics));
    }

    (groups, skipped)
}

/// What one metric came to. The empty case is spelled out rather than folded
/// into `Unsupported` because the two are different diagnoses: one says this
/// build never handled that metric kind, the other says the points inside a
/// kind it does handle have changed shape -- and the second is the one that
/// means "a VS Code update moved the spool under us".
enum Translated {
    Metric(Value, usize),
    /// Every point was dropped. Nothing to post -- an OTLP metric with an
    /// empty `dataPoints` is a round trip that tells the collector nothing --
    /// but the points still count as lost.
    NoPoints(usize),
    Unsupported,
}

/// One `Metric`, the data points it dropped, or the reason there is no metric.
fn metric(metric: &Metric) -> Translated {
    let temporality = temporality(metric.aggregation_temporality);
    let Some(kind) = metric.data_point_type else {
        return Translated::Unsupported;
    };
    let (field, data, dropped, kept) = match kind {
        JS_HISTOGRAM => {
            let (data_points, dropped) = points::histogram(metric);
            let kept = data_points.len();
            let data = json!({
                "dataPoints": data_points,
                "aggregationTemporality": temporality,
            });
            ("histogram", data, dropped, kept)
        }
        JS_GAUGE => {
            let (data_points, dropped) = points::number(metric);
            let kept = data_points.len();
            ("gauge", json!({ "dataPoints": data_points }), dropped, kept)
        }
        JS_SUM => {
            let (data_points, dropped) = points::number(metric);
            let kept = data_points.len();
            let data = json!({
                "dataPoints": data_points,
                "aggregationTemporality": temporality,
                "isMonotonic": metric.is_monotonic.unwrap_or(false),
            });
            ("sum", data, dropped, kept)
        }
        _ => return Translated::Unsupported,
    };

    if kept == 0 {
        return Translated::NoPoints(dropped);
    }

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
    Translated::Metric(Value::Object(object), dropped)
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
