//! Data points: the level at which a record can be *partly* wrong.
//!
//! A line that will not parse at all is already counted. These are the cases
//! that used to slip between the counters -- a point silently dropped inside a
//! record that still reported as translated, and a point shipped in a shape
//! the OTLP spec forbids.

use super::*;

/// OTLP requires `len(bucketCounts) == len(explicitBounds) + 1`. A payload
/// that breaks it is rejected by a validating collector, and because the
/// checkpoint only moves after a 2xx, the same bytes rebuild the same invalid
/// payload on every later wake -- the drain stops at that offset for good.
#[test]
fn a_histogram_whose_buckets_do_not_match_its_bounds_is_not_shipped() {
    let line = metric_line(
        json!({ "name": "gen_ai.client.token.usage", "valueType": 1 }),
        0,
        json!({
            "count": 3,
            "sum": 764,
            // Three bounds need four counts. This one has three.
            "buckets": { "boundaries": [1, 4, 16], "counts": [0, 1, 2] },
        }),
    );

    let built = batch::build(&[line]);

    assert!(
        built.metrics.is_none(),
        "an invalid histogram must not reach the collector: {:?}",
        built.metrics
    );
    assert_eq!(
        built.counts.dropped_points,
        1,
        "and the point it cost must be counted: {}",
        built.counts.describe()
    );
}

/// The same invariant from the other side: `counts` absent (serde defaults it
/// to empty) with `boundaries` present is the shape a Copilot release that
/// renames one field produces.
#[test]
fn a_histogram_with_no_bucket_counts_at_all_is_not_shipped() {
    let line = metric_line(
        json!({ "name": "gen_ai.client.token.usage", "valueType": 1 }),
        0,
        json!({ "count": 3, "buckets": { "boundaries": [1, 4] } }),
    );

    let built = batch::build(&[line]);

    assert!(built.metrics.is_none(), "{:?}", built.metrics);
    assert_eq!(
        built.counts.dropped_points,
        1,
        "{}",
        built.counts.describe()
    );
}

/// A histogram with no buckets at all is legal: zero bounds, one count.
#[test]
fn a_single_bucket_histogram_is_valid_and_ships() {
    let line = metric_line(
        json!({ "name": "gen_ai.client.token.usage", "valueType": 1 }),
        0,
        json!({ "count": 3, "buckets": { "boundaries": [], "counts": [3] } }),
    );

    let built = batch::build(&[line]);
    let payload = built.metrics.unwrap_or(Value::Null);

    assert_eq!(
        parse(
            &payload,
            "/resourceMetrics/0/scopeMetrics/0/metrics/0/histogram/dataPoints/0/bucketCounts"
        ),
        json!(["3"])
    );
    assert_eq!(
        built.counts.dropped_points,
        0,
        "{}",
        built.counts.describe()
    );
}

/// A `value` that is not a number at all: the point cannot be translated, and
/// the record it came from must not report as a clean translation. The tally
/// is the module's own early warning; a drop that does not move it is exactly
/// the silence it exists to prevent.
#[test]
fn a_number_point_whose_value_is_not_a_number_is_counted_not_swallowed() {
    let line = metric_line(
        json!({ "name": "copilot_chat.session.count", "valueType": 1 }),
        3,
        json!({ "n": 1 }),
    );

    let built = batch::build(&[line]);

    assert_eq!(
        built.counts.dropped_points,
        1,
        "{}",
        built.counts.describe()
    );
    assert!(
        built.metrics.is_none(),
        "an empty `sum.dataPoints` is not something to post: {:?}",
        built.metrics
    );
    assert_eq!(
        built.counts.metrics, 0,
        "a record that produced no point is not a translated record"
    );
}

/// `valueType: 0` (INT) with a value JSON spells as a float. `as_i64` rejects
/// `1.0`, which used to drop the point.
#[test]
fn an_integral_float_under_value_type_int_is_kept() {
    let line = metric_line(
        json!({ "name": "copilot_chat.session.count", "valueType": 0 }),
        3,
        json!(1.0),
    );

    let built = batch::build(&[line]);
    let payload = built.metrics.unwrap_or(Value::Null);

    assert_eq!(
        parse(
            &payload,
            "/resourceMetrics/0/scopeMetrics/0/metrics/0/sum/dataPoints/0/asInt"
        ),
        json!("1")
    );
    assert_eq!(
        built.counts.dropped_points,
        0,
        "{}",
        built.counts.describe()
    );
}

/// A genuinely fractional value under `valueType: 0` keeps its precision
/// instead of being truncated into `asInt` or dropped: OTLP lets a
/// `NumberDataPoint` carry either, and the descriptor is a hint about the
/// instrument, not a licence to round the measurement.
#[test]
fn a_fractional_value_under_value_type_int_keeps_its_precision() {
    let line = metric_line(
        json!({ "name": "copilot_chat.session.count", "valueType": 0 }),
        3,
        json!(1.5),
    );

    let built = batch::build(&[line]);
    let payload = built.metrics.unwrap_or(Value::Null);

    assert_eq!(
        parse(
            &payload,
            "/resourceMetrics/0/scopeMetrics/0/metrics/0/sum/dataPoints/0/asDouble"
        ),
        json!(1.5)
    );
    assert_eq!(
        built.counts.dropped_points,
        0,
        "{}",
        built.counts.describe()
    );
}

/// The `{}` records Copilot's exporter really writes are NOT loss. Counting
/// them as discarded would paint 22 of every 98 records red and train the
/// reader to ignore the row -- which is how a real parser regression stays
/// invisible.
#[test]
fn empty_records_are_not_counted_as_loss() {
    let built = batch::build(&["{}".to_owned(), "{}".to_owned(), log_line()]);

    assert_eq!(built.counts.empty, 2, "{}", built.counts.describe());
    assert_eq!(built.counts.discarded(), 0, "{}", built.counts.describe());
}

/// A record that is neither shape and not empty IS loss: this is the shape a
/// Copilot release that renames `_body`/`hrTime` produces.
#[test]
fn an_unrecognised_record_counts_as_loss() {
    let built = batch::build(&[json!({ "body": "x", "time": [1, 2] }).to_string()]);

    assert_eq!(built.counts.unknown, 1, "{}", built.counts.describe());
    assert_eq!(built.counts.discarded(), 1, "{}", built.counts.describe());
}
