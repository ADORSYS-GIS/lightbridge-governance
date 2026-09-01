//! The shapes GitHub Copilot's file exporter actually writes.
//!
//! ⚠️ **This file is not OTLP.** With
//! `github.copilot.chat.otel.exporterType = "file"`, VS Code's Copilot Chat
//! serialises the OpenTelemetry **JS SDK's own in-memory object graph**, one
//! JSON object per line -- private `_`-prefixed fields and all. Nothing here
//! is a wire format anybody promised to keep stable, which is why every field
//! below is optional or `#[serde(default)]` and why `_rawAttributes` is kept
//! as raw [`Value`]s rather than a typed pair.
//!
//! The rule this encodes: a record shape we do not recognise is **skipped and
//! counted**, never fatal. A Copilot release that renames `_body` must cost
//! the developer some telemetry, not every subsequent run of the drain.
//!
//! Field spellings verified against a live spool (98 records, Copilot Chat
//! 0.62.0). Enum integers are the JS SDK's, not OTLP's -- see
//! [`super::metrics`] for the translation.

use serde::Deserialize;
use serde_json::{Map, Value};

/// Which shape a line carries. Lives in [`super::classify`] only because this
/// file would otherwise be over the 200-LoC ceiling; it is *about* the shapes
/// below, so it stays reachable as `record::classify`.
pub use super::classify::{Kind, classify};

/// The JS SDK's `HrTime`: `[seconds, nanoseconds]`. A `Vec`, not `[i64; 2]`,
/// so a record carrying a differently-shaped array degrades to "no timestamp"
/// instead of failing the whole line's parse.
pub type HrTime = Vec<i64>;

/// A serialised `Resource`. Only `_rawAttributes` is read; the sibling
/// `_asyncAttributesPending` describes SDK-internal bookkeeping that has no
/// OTLP counterpart.
#[derive(Debug, Default, Deserialize)]
pub struct Resource {
    /// `[[key, value], ...]`. Deliberately `Vec<Value>` and not
    /// `Vec<(String, Value)>`: serde would reject the *entire record* over
    /// one entry that grew a third element, and losing a whole session's
    /// telemetry to a resource-attribute tweak is exactly the fragility this
    /// module exists to avoid. Malformed entries are dropped individually in
    /// [`super::otlp::resource_attributes`].
    #[serde(default, rename = "_rawAttributes")]
    pub raw_attributes: Vec<Value>,
}

/// An instrumentation scope, spelled `scope` on metrics lines and
/// `instrumentationScope` on log lines.
#[derive(Debug, Default, Deserialize)]
pub struct Scope {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

/// A metrics line: one `ResourceMetrics` as the JS SDK holds it.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsRecord {
    #[serde(default)]
    pub resource: Resource,
    #[serde(default)]
    pub scope_metrics: Vec<ScopeMetrics>,
}

#[derive(Debug, Deserialize)]
pub struct ScopeMetrics {
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub metrics: Vec<Metric>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metric {
    #[serde(default)]
    pub descriptor: Descriptor,
    /// The JS SDK's `DataPointType`, not OTLP's. `None` (absent) is treated
    /// the same as an unknown value: skipped and counted.
    pub data_point_type: Option<i64>,
    /// The JS SDK's `AggregationTemporality`, not OTLP's.
    pub aggregation_temporality: Option<i64>,
    #[serde(default)]
    pub data_points: Vec<DataPoint>,
    pub is_monotonic: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub unit: String,
    /// The JS SDK's `ValueType` (`INT = 0`, `DOUBLE = 1`). Decides whether a
    /// number data point is emitted as OTLP's `asInt` or `asDouble`.
    pub value_type: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataPoint {
    #[serde(default)]
    pub attributes: Map<String, Value>,
    pub start_time: Option<HrTime>,
    pub end_time: Option<HrTime>,
    /// A bare number for a sum/gauge, or a histogram object. Left as a
    /// [`Value`] because which one it is depends on the sibling
    /// `dataPointType`, and a wrong guess must skip one metric rather than
    /// fail the line.
    #[serde(default)]
    pub value: Value,
}

/// The histogram payload inside [`DataPoint::value`] when the metric's
/// `dataPointType` is `HISTOGRAM`.
#[derive(Debug, Deserialize)]
pub struct HistogramValue {
    pub count: Option<u64>,
    pub sum: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    #[serde(default)]
    pub buckets: Buckets,
}

#[derive(Debug, Default, Deserialize)]
pub struct Buckets {
    #[serde(default)]
    pub boundaries: Vec<f64>,
    #[serde(default)]
    pub counts: Vec<u64>,
}

/// A log line: one `ReadableLogRecord`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogRecord {
    #[serde(rename = "_body")]
    pub body: Option<Value>,
    #[serde(default)]
    pub attributes: Map<String, Value>,
    pub hr_time: Option<HrTime>,
    pub hr_time_observed: Option<HrTime>,
    #[serde(default)]
    pub instrumentation_scope: Scope,
    #[serde(default)]
    pub resource: Resource,
    pub span_context: Option<SpanContext>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanContext {
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub trace_flags: Option<u32>,
}
