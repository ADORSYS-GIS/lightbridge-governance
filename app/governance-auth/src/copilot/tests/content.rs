//! A record that parses but carries nothing is **lost**, not delivered.
//!
//! The stated invariant is "delivered or recorded as lost", and there was a
//! third outcome hiding between them: **delivered empty**.
//!
//! [`record::classify`] dispatched a line to the log shape on `_body` *or*
//! `hrTime`. A Copilot release renaming only `_body` -- strictly smaller, and
//! therefore likelier, than the two-field rename already covered -- still
//! matched on `hrTime`, still parsed (every field of `LogRecord` is optional),
//! and still exported. What arrived at the collector was a log record with a
//! timestamp and attributes and no body at all. Nothing counted it, `status`
//! stayed green, and the content of every log line was gone.
//!
//! The same hole exists one shape over: a metrics line whose `dataPoints` key
//! moved parses into a metric with no points, contributes nothing to the
//! export, and is skipped without incrementing anything -- because the
//! per-point counters only move for points that were *present* and bad.

use serde_json::json;

use super::*;

fn log_without(field: &str) -> String {
    let mut line: Value = serde_json::from_str(&log_line()).expect("the log fixture parses");
    if let Some(object) = line.as_object_mut() {
        object.remove(field);
    }
    line.to_string()
}

/// THE regression test. `hrTime` survives the rename, so the record is still
/// recognisably a log line -- and it has nothing left to say.
#[test]
fn a_log_record_whose_body_field_was_renamed_is_counted_as_lost() {
    let drifted = log_without("_body");
    let built = batch::build(&[drifted]);

    assert_eq!(
        built.counts.logs, 0,
        "a log record with no body carries nothing; exporting it delivers an empty envelope and \
         reports success"
    );
    assert_eq!(
        built.counts.unknown, 1,
        "and it must land in the counter `status` turns red on"
    );
    assert!(
        built.logs.is_none(),
        "there must be no logs payload at all, got: {:?}",
        built.logs
    );
    assert_eq!(built.counts.discarded(), 1);
}

/// The guard on the rule above: an ordinary log record must not be swept up by
/// it. Without this, "count the empty ones" could be satisfied by counting
/// everything.
#[test]
fn an_ordinary_log_record_is_still_delivered() {
    let built = batch::build(&[log_line()]);
    assert_eq!(built.counts.logs, 1);
    assert_eq!(built.counts.unknown, 0);
    assert_eq!(built.counts.discarded(), 0);
}

/// A log record that lost its timestamp but kept its body is still a log
/// record -- OTLP treats an absent `timeUnixNano` as unset, and the attributes
/// and body are the governance signal. Dispatching on `_body` alone must not
/// have narrowed this away.
#[test]
fn a_log_record_that_only_lost_its_timestamp_is_still_delivered() {
    let built = batch::build(&[log_without("hrTime")]);
    assert_eq!(built.counts.logs, 1, "the body is what carries the record");
    assert_eq!(built.counts.unknown, 0);
}

/// The metrics-side twin. `dataPoints` renamed: the descriptor and the type
/// are still there, so this is recognisably a metrics line, and it produces
/// exactly nothing.
#[test]
fn a_metrics_record_that_yields_no_metrics_at_all_is_counted_as_lost() {
    let drifted = json!({
        "resource": { "_rawAttributes": [["service.name", "copilot-chat"]] },
        "scopeMetrics": [{
            "scope": { "name": "copilot-chat" },
            "metrics": [{
                "descriptor": { "name": "copilot_chat.session.count", "valueType": 1 },
                "aggregationTemporality": 1,
                "dataPointType": 3,
                // Was `dataPoints`.
                "points": [{ "attributes": {}, "value": 1 }],
                "isMonotonic": true,
            }],
        }],
    })
    .to_string();

    let built = batch::build(&[drifted]);
    assert_eq!(built.counts.metrics, 0);
    assert_eq!(
        built.counts.discarded(),
        1,
        "a metrics line that declared a metric and produced none is loss, not a no-op: {:?}",
        built.counts
    );
    assert!(built.metrics.is_none());
}

/// And the guard: a line that never declared a metric has nothing to lose, so
/// it must not move the counter. Folding these in is how a loss counter stops
/// meaning anything -- see [`record::classify`]'s note on the 22-in-98 `{}`
/// records.
#[test]
fn a_metrics_record_that_declared_nothing_is_not_counted_as_loss() {
    let empty = json!({
        "resource": { "_rawAttributes": [] },
        "scopeMetrics": [{ "scope": { "name": "copilot-chat" }, "metrics": [] }],
    })
    .to_string();

    let built = batch::build(&[empty]);
    assert_eq!(built.counts.discarded(), 0, "{:?}", built.counts);
    assert_eq!(built.counts.unknown, 0);
}
