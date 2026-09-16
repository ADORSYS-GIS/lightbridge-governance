//! Tests for the run-level and status gauges.

use opentelemetry_sdk::metrics::data::ResourceMetrics;

use super::{
    StatusRecording, record_run_metrics,
    test_util::{export, outcome, u64_gauge_points},
};
use crate::sync::SyncStatus;

/// The core assertion the review demanded: `governance.copilot.
/// last_run_timestamp_seconds` (the `governance.copilot.run` counter's
/// replacement) must be a Gauge carrying the actual unix timestamp, not a
/// Sum. Confirmed against the pre-fix code (see report): building this
/// instrument with `meter.u64_counter(...)` instead makes `u64_gauge_points`
/// panic with "expected ... to be a u64 Gauge, got U64(Sum(...))", because a
/// Counter is exported as `AggregatedMetrics::U64(MetricData::Sum(..))`, not
/// `Gauge`.
#[test]
fn last_run_timestamp_is_a_gauge_carrying_the_unix_timestamp() {
    let resource_metrics = export(|meter| {
        record_run_metrics(meter, "sync", &[], 0, 1_700_000_000);
    });

    let points = u64_gauge_points(
        &resource_metrics,
        "governance.copilot.last_run_timestamp_seconds",
    );
    assert_eq!(
        points,
        vec![(
            vec![("command".to_owned(), "sync".to_owned())],
            1_700_000_000
        )]
    );
}

/// `days`, `reports` and `rows` must all be Gauges too -- the same defect
/// applied to all four series the old code pushed as counters.
#[test]
fn days_reports_and_rows_are_all_gauges() {
    let outcomes = [outcome("organization-1-day", "ok", 3)];
    let resource_metrics = export(|meter| {
        record_run_metrics(meter, "sync", &outcomes, 1, 1_700_000_000);
    });

    assert_eq!(
        u64_gauge_points(&resource_metrics, "governance.copilot.days"),
        vec![(vec![], 1)]
    );
    assert_eq!(
        u64_gauge_points(&resource_metrics, "governance.copilot.reports"),
        vec![(
            vec![
                ("report".to_owned(), "organization-1-day".to_owned()),
                ("status".to_owned(), "ok".to_owned())
            ],
            1
        )]
    );
    assert_eq!(
        u64_gauge_points(&resource_metrics, "governance.copilot.rows"),
        vec![(
            vec![("report".to_owned(), "organization-1-day".to_owned())],
            3
        )]
    );
}

/// A backfill run ingests several days per report type. Because a gauge's
/// `record()` overwrites (last-value-wins) rather than sums for an identical
/// attribute set within one collection cycle, recording each day's outcome
/// directly (the naive counter->gauge swap) would silently drop every day but
/// the last. This proves the pre-aggregation in `record_run_metrics` actually
/// happened: two days of the same report/status must sum to a `reports` value
/// of 2 and a `rows` value of 3 + 5 = 8, not whichever day was recorded last.
///
/// Confirmed against a deliberately un-aggregated version (recording straight
/// from the `outcomes` loop instead of `reports_by_key`/`rows_by_report`): it
/// failed with `reports` = 1 and `rows` = 5 (the second day's values only),
/// exactly the silent-data-loss mechanism this test exists to catch.
#[test]
fn multiple_days_for_the_same_report_are_summed_not_overwritten() {
    let outcomes = [
        outcome("organization-1-day", "ok", 3),
        outcome("organization-1-day", "ok", 5),
    ];
    let resource_metrics = export(|meter| {
        record_run_metrics(meter, "sync", &outcomes, 2, 1_700_000_000);
    });

    assert_eq!(
        u64_gauge_points(&resource_metrics, "governance.copilot.reports"),
        vec![(
            vec![
                ("report".to_owned(), "organization-1-day".to_owned()),
                ("status".to_owned(), "ok".to_owned())
            ],
            2
        )]
    );
    assert_eq!(
        u64_gauge_points(&resource_metrics, "governance.copilot.rows"),
        vec![(
            vec![("report".to_owned(), "organization-1-day".to_owned())],
            8
        )]
    );
}

/// Prometheus convention reserves the `_total` suffix for counters. Every
/// series `record_run_metrics` emits is now a gauge, so none of them may
/// carry it -- a dashboard author reading the name alone must not be misled
/// into reaching for `rate()`/`increase()`.
#[test]
fn no_run_metric_name_carries_a_counter_style_total_suffix() {
    let outcomes = [outcome("organization-1-day", "ok", 1)];
    let resource_metrics = export(|meter| {
        record_run_metrics(meter, "sync", &outcomes, 1, 1_700_000_000);
    });

    let names: Vec<&str> = resource_metrics
        .iter()
        .flat_map(ResourceMetrics::scope_metrics)
        .flat_map(|sm| sm.metrics())
        .map(|m| m.name())
        .collect();
    assert_eq!(names.len(), 4, "expected exactly the four run-level series");
    for name in names {
        assert!(
            !name.ends_with("_total"),
            "{name} reads as a counter to Prometheus convention but is recorded as a gauge"
        );
    }
}

/// BLOCKER 3, the core assertion: a never-synced deployment must not compute
/// the same age as one that just succeeded. Before this fix, `run_status`
/// returned the sentinel `(-1, -1)` and `push_status_metrics` did
/// `age_days.max(0) as u64 * 86_400`, which folds `-1` into `0` -- identical
/// to `SyncStatus::Synced { age_days: 0, .. }`.
#[test]
fn never_synced_does_not_compute_the_same_recording_as_synced_zero_days_ago() {
    let never = StatusRecording::from(SyncStatus::NeverSynced);
    let just_succeeded = StatusRecording::from(SyncStatus::Synced {
        age_days: 0,
        unmapped_users: 0,
    });
    assert_ne!(never, just_succeeded);
}

/// The age gauge must be omitted (no data point), not a fake zero -- that is
/// the entire fix, stated as directly as possible.
#[test]
fn never_synced_omits_the_age_gauge_entirely() {
    let recording = StatusRecording::from(SyncStatus::NeverSynced);
    assert_eq!(recording.age_seconds, None);
    assert_eq!(recording.ever_synced, 0);
}

#[test]
fn synced_records_ever_synced_and_the_age_in_seconds() {
    let recording = StatusRecording::from(SyncStatus::Synced {
        age_days: 2,
        unmapped_users: 5,
    });
    assert_eq!(recording.ever_synced, 1);
    assert_eq!(recording.age_seconds, Some(2 * 86_400));
    assert_eq!(recording.unmapped_users, 5);
}

/// A negative age (clock skew, or a report_day briefly in the future) must
/// clamp to zero, not underflow the `u64` cast.
#[test]
fn synced_clamps_a_negative_age_to_zero() {
    let recording = StatusRecording::from(SyncStatus::Synced {
        age_days: -1,
        unmapped_users: 0,
    });
    assert_eq!(recording.age_seconds, Some(0));
}
