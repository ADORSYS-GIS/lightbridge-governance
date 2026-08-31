//! Spool lines -> the two OTLP payloads, plus an honest tally of what did not
//! make it.
//!
//! Nothing in here returns `Err`. A line that will not parse, or whose shape
//! this build does not recognise, is counted and dropped -- the private
//! `_`-prefixed fields it is made of are SDK internals that change between
//! Copilot releases, and one renamed field must not stop the drain for every
//! other record in the file.
//!
//! ## Counted is not the same as reported
//!
//! [`Counts::describe`] prints on every run, but stderr from a systemd timer
//! is not somewhere anybody looks. So [`Counts::discarded`] is the number that
//! is *persisted* into the checkpoint and surfaced by `status`: the drain is
//! allowed to give up on a record, and is not allowed to do it quietly.
//!
//! The split between [`Counts::empty`] and [`Counts::unknown`] is what keeps
//! that number meaningful -- see [`record::classify`].

use serde_json::Value;

use super::{
    logs, metrics, otlp, push,
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
    /// Lines that are JSON, are not empty, and match neither known shape.
    /// This is the counter a Copilot release that renames `_body` moves.
    pub unknown: usize,
    /// The literal `{}` records Copilot's exporter really does write (22 of 98
    /// on the sample this parser was built against). Not loss.
    pub empty: usize,
    /// Individual metrics dropped for an unsupported `dataPointType`.
    pub unsupported_metrics: usize,
    /// Individual data points dropped inside a metric this build *does*
    /// translate -- a value that is not a number, or a histogram whose buckets
    /// break OTLP's invariant. Counted separately because the record around
    /// them may well have been exported.
    pub dropped_points: usize,
    /// Records the collector permanently refused, one at a time, after taking
    /// others from the same batch. Filled in by [`super::export`], not here.
    pub rejected: usize,
}

impl Counts {
    /// Records that were consumed and will never reach the collector.
    ///
    /// This is the number `status` turns non-green on, so what it excludes
    /// matters as much as what it includes: `empty` is absent because those
    /// records carry nothing, and `metrics`/`logs` are absent because they
    /// were delivered.
    pub fn discarded(&self) -> u64 {
        let total = self
            .unparsable
            .saturating_add(self.unknown)
            .saturating_add(self.unsupported_metrics)
            .saturating_add(self.dropped_points)
            .saturating_add(self.rejected);
        u64::try_from(total).unwrap_or(u64::MAX)
    }

    /// One line for stderr. Always printed, including the zeros: "0 lost" is
    /// the reassurance, and a number that starts climbing after a VS Code
    /// update is the early warning that this parser needs revisiting.
    pub fn describe(&self) -> String {
        format!(
            "{} metric record(s), {} log record(s); {} empty; discarded {} ({} unparsable, {} \
             unrecognised, {} unsupported metric(s), {} bad data point(s))",
            self.metrics,
            self.logs,
            self.empty,
            self.discarded(),
            self.unparsable,
            self.unknown,
            self.unsupported_metrics,
            self.dropped_points,
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

impl Batch {
    /// The payload for one signal and how many records of that signal it
    /// carries. `(None, 0)` means "nothing to post", which is not a failure.
    pub fn signal(self, signal: push::Signal) -> (Option<Value>, usize) {
        match signal {
            push::Signal::Metrics => (self.metrics, self.counts.metrics),
            push::Signal::Logs => (self.logs, self.counts.logs),
        }
    }
}

/// Generic over the element type so callers can pass either the owned lines a
/// test writes or the borrowed subset [`super::export`] bisects, without
/// either side cloning a batch's worth of strings to satisfy the signature.
pub fn build<S: AsRef<str>>(lines: &[S]) -> Batch {
    let mut counts = Counts::default();
    let mut metric_groups: Vec<otlp::Grouped> = Vec::new();
    let mut log_groups: Vec<otlp::Grouped> = Vec::new();

    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(line.as_ref()) else {
            counts.unparsable = counts.unparsable.saturating_add(1);
            continue;
        };

        match record::classify(&value) {
            Kind::Metrics => match serde_json::from_value::<MetricsRecord>(value) {
                Ok(parsed) => {
                    let (groups, skipped) = metrics::transform(&parsed);
                    counts.unsupported_metrics = counts
                        .unsupported_metrics
                        .saturating_add(skipped.unsupported);
                    counts.dropped_points = counts.dropped_points.saturating_add(skipped.points);
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
            Kind::Empty => counts.empty = counts.empty.saturating_add(1),
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
