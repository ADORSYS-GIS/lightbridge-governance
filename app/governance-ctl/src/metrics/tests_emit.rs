//! Tests for the OTLP emit-outcome gauges (AC 7).

use super::{
    record_emit_metrics,
    test_util::{export, u64_gauge_points},
};

/// AC 7: a partial accept (some records rejected) must surface as an error
/// metric (`emit_partial_accept = 1`), not be swallowed. A fully accepted
/// batch must read `0`.
#[test]
fn partial_accept_is_surfaced_as_an_error_metric() {
    let resource_metrics = export(|meter| {
        record_emit_metrics(meter, &[("organization-1-day".to_owned(), 3)], 1);
    });
    let points = u64_gauge_points(&resource_metrics, "governance.copilot.emit_partial_accept");
    assert_eq!(
        points,
        vec![(vec![], 1)],
        "a rejected record must set the error gauge"
    );

    let resource_metrics = export(|meter| {
        record_emit_metrics(meter, &[("organization-1-day".to_owned(), 3)], 0);
    });
    let points = u64_gauge_points(&resource_metrics, "governance.copilot.emit_partial_accept");
    assert_eq!(
        points,
        vec![(vec![], 0)],
        "a fully accepted batch must read 0"
    );
}

/// The `report` attribute of a gauge point, for order-independent comparison.
fn report_label(p: &(Vec<(String, String)>, u64)) -> &str {
    p.0.iter()
        .find(|(k, _)| k == "report")
        .map_or("", |(_, v)| v.as_str())
}

/// Sort gauge points by their `report` attribute so an assertion is
/// order-independent -- the SDK stores data points in a HashMap, so iteration
/// order is not guaranteed.
fn sort_by_report(points: &mut [(Vec<(String, String)>, u64)]) {
    points.sort_by(|a, b| report_label(a).cmp(report_label(b)));
}

/// AC 7: `emit_accepted_rows` reflects the records the collector accepted, by
/// report.
#[test]
fn emit_accepted_rows_reflects_accepted_records_by_report() {
    let resource_metrics = export(|meter| {
        record_emit_metrics(
            meter,
            &[
                ("organization-1-day".to_owned(), 3),
                ("users-1-day".to_owned(), 5),
            ],
            0,
        );
    });
    let mut points = u64_gauge_points(&resource_metrics, "governance.copilot.emit_accepted_rows");
    let mut expected = vec![
        (
            vec![("report".to_owned(), "organization-1-day".to_owned())],
            3,
        ),
        (vec![("report".to_owned(), "users-1-day".to_owned())], 5),
    ];
    sort_by_report(&mut points);
    sort_by_report(&mut expected);
    assert_eq!(points, expected);
}

/// The gauge-collapse regression (adversarial review): a backfill run emits
/// several days per report type, so `accepted_by_report` carries several
/// entries sharing a `report` label. A gauge's `record()` overwrites
/// (last-value-wins) for an identical attribute set, so recording each entry
/// directly would report only the final day's count instead of the run total.
/// This proves the per-report pre-aggregation in `record_emit_metrics`
/// happened: two days of the same report must sum to 3 + 5 = 8, not whichever
/// day was recorded last.
///
/// Confirmed against a deliberately un-aggregated version (recording straight
/// from the `accepted_by_report` loop): it failed with a single point of value
/// 5 (the second day only), exactly the silent-undercount this test exists to
/// catch.
#[test]
fn emit_accepted_rows_sums_duplicate_report_labels() {
    let resource_metrics = export(|meter| {
        record_emit_metrics(
            meter,
            &[
                ("organization-1-day".to_owned(), 3),
                ("organization-1-day".to_owned(), 5),
            ],
            0,
        );
    });
    let points = u64_gauge_points(&resource_metrics, "governance.copilot.emit_accepted_rows");
    assert_eq!(
        points,
        vec![(
            vec![("report".to_owned(), "organization-1-day".to_owned())],
            8
        )],
        "two days of the same report must sum to the run total, not collapse to the last day"
    );
}
