//! Spool lines -> the two OTLP payloads, plus an honest tally of what did not
//! make it.
//!
//! Nothing in here returns `Err`. A line that will not parse, or whose shape
//! this build does not recognise, is counted and dropped -- the private
//! `_`-prefixed fields it is made of are SDK internals that change between
//! Copilot releases, and one renamed field must not stop the drain for every
//! other record in the file. The counts are what makes that visible instead
//! of silent: [`Counts::describe`] is printed on every run.

use serde_json::Value;

use super::{
    logs, metrics, otlp,
    record::{self, Kind, LogRecord, MetricsRecord},
};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Metrics lines whose transform produced at least one metric.
    pub metrics: usize,
    /// Log lines transformed.
    pub logs: usize,
    /// Lines that are not JSON at all.
    pub unparsable: usize,
    /// Lines that are JSON but match neither known shape -- including the
    /// literal `{}` records Copilot's exporter really does write (22 of 98 on
    /// the sample this parser was built against).
    pub unknown: usize,
    /// Individual metrics dropped for an unsupported `dataPointType`.
    pub unsupported_metrics: usize,
}

impl Counts {
    pub fn total_pushed(&self) -> u64 {
        u64::try_from(self.metrics.saturating_add(self.logs)).unwrap_or(u64::MAX)
    }

    /// One line for stderr. Always printed, including the zeros: "0 skipped"
    /// is the reassurance, and a number that starts climbing after a VS Code
    /// update is the early warning that this parser needs revisiting.
    pub fn describe(&self) -> String {
        format!(
            "{} metric record(s), {} log record(s); skipped {} unparsable, {} unrecognised, {} \
             unsupported metric(s)",
            self.metrics, self.logs, self.unparsable, self.unknown, self.unsupported_metrics
        )
    }
}

#[derive(Debug, Default)]
pub struct Batch {
    /// `{"resourceMetrics": [...]}`, or `None` when there is nothing to send.
    pub metrics: Option<Value>,
    /// `{"resourceLogs": [...]}`, or `None`.
    pub logs: Option<Value>,
    pub counts: Counts,
}

pub fn build(lines: &[String]) -> Batch {
    let mut counts = Counts::default();
    let mut metric_groups: Vec<otlp::Grouped> = Vec::new();
    let mut log_groups: Vec<otlp::Grouped> = Vec::new();

    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            counts.unparsable = counts.unparsable.saturating_add(1);
            continue;
        };

        match record::classify(&value) {
            Kind::Metrics => match serde_json::from_value::<MetricsRecord>(value) {
                Ok(parsed) => {
                    let (groups, unsupported) = metrics::transform(&parsed);
                    counts.unsupported_metrics =
                        counts.unsupported_metrics.saturating_add(unsupported);
                    if groups.iter().any(|(_, _, items)| !items.is_empty()) {
                        counts.metrics = counts.metrics.saturating_add(1);
                    }
                    metric_groups.extend(groups);
                }
                // Classified as metrics but the inner shape moved. Same
                // treatment as any other record we cannot read.
                Err(_) => counts.unknown = counts.unknown.saturating_add(1),
            },
            Kind::Log => match serde_json::from_value::<LogRecord>(value) {
                Ok(parsed) => {
                    counts.logs = counts.logs.saturating_add(1);
                    log_groups.push(logs::transform(&parsed));
                }
                Err(_) => counts.unknown = counts.unknown.saturating_add(1),
            },
            Kind::Unknown => counts.unknown = counts.unknown.saturating_add(1),
        }
    }

    Batch {
        metrics: payload(metric_groups, "scopeMetrics", "metrics", "resourceMetrics"),
        logs: payload(log_groups, "scopeLogs", "logRecords", "resourceLogs"),
        counts,
    }
}

/// `None` rather than an empty `resourceMetrics` array: posting an empty
/// export is a request that costs a round trip and tells the collector
/// nothing, and `None` is what lets the caller skip the POST entirely.
fn payload(
    groups: Vec<otlp::Grouped>,
    scope_field: &str,
    item_field: &str,
    resource_field: &str,
) -> Option<Value> {
    let grouped = otlp::group(groups, scope_field, item_field);
    if grouped.is_empty() {
        return None;
    }
    // Built by hand rather than with `json!`, whose keys must be literals.
    let mut object = serde_json::Map::new();
    object.insert(resource_field.to_owned(), Value::Array(grouped));
    Some(Value::Object(object))
}
