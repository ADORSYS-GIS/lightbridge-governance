//! Operational metrics pushed to Mimir over OTLP at the end of a run.
//!
//! The collector runs as a CronJob pod, which cannot be scraped (ADR-0007's
//! ServiceMonitor pattern is for the always-on API). So this is a push, not a
//! scrape: ~10 low-cardinality health metrics via the cluster's Alloy OTLP
//! endpoint. Business data lives in Postgres and is read from there -- these
//! counters say "is the collector healthy and current", nothing more.
//!
//! A failed metric push is a WARN, not a run failure: the sync itself either
//! landed rows or it did not, and the CronJob's exit code + logs are the
//! authoritative health signal. Failing the run on a metrics hiccup would
//! manufacture an outage that never happened.

use std::collections::HashMap;

use anyhow::Result;
use opentelemetry::{
    KeyValue,
    metrics::{Meter, MeterProvider},
};
use opentelemetry_otlp::WithExportConfig;
use tracing::warn;

use crate::sync::SyncStatus;

/// Endpoint env var (standard OTel naming); absent = push skipped, loudly --
/// an operator who forgot to set this otherwise gets a CronJob that looks
/// healthy (exit 0, no error) while pushing no metrics at all.
pub fn endpoint_from_env() -> Option<String> {
    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .ok()
        .filter(|e| !e.is_empty());
    if endpoint.is_none() {
        warn!(
            "OTEL_EXPORTER_OTLP_ENDPOINT not set or empty; skipping metrics push -- \
             dashboards/alerts fed by this run will not update"
        );
    }
    endpoint
}

/// Records the run-level gauges (`last_run_timestamp_seconds`, `days`,
/// `reports` by report/status, `rows` by report) into `meter`, as of one run.
///
/// These are gauges, not counters, because `push` (below) builds a brand-new
/// `SdkMeterProvider` for every invocation and the process exits right after
/// -- no aggregation state survives between runs. A counter under those
/// conditions is not a counter at all: `governance.copilot.run` used to be a
/// `u64_counter` incremented once per run, so every run pushed a fresh series
/// whose cumulative value was permanently `1`; `increase()`/`rate()` across
/// two runs saw `1 -> 1`, indistinguishable from a job that ran once and
/// died. The `days`/`reports`/`rows` counters had the same defect with
/// noisier symptoms: the value pushed depended on how much a single run
/// happened to cover, so a scrape sometimes saw a coincidental increase
/// (undercounting) and sometimes a decrease (PromQL guessing a false reset).
/// A gauge holding "value as of the last run" is what these numbers actually
/// are, and it is exactly what Mimir needs to keep the series graphable
/// across restarts of this one-shot process.
///
/// `now_unix` is threaded through explicitly (rather than read via
/// `chrono::Utc::now()` inside this function) so tests can assert the exact
/// timestamp recorded without racing the clock.
fn record_run_metrics(
    meter: &Meter,
    command: &str,
    outcomes: &[governance_copilot::ReportOutcome],
    days: u64,
    now_unix: i64,
) {
    // Replaces the old `governance.copilot.run` counter, which was
    // permanently `1` as a counter and would have been permanently `1` as a
    // gauge too -- useless either way. A timestamp gauge is genuinely useful:
    // `time() - governance_copilot_last_run_timestamp_seconds` is a real
    // freshness signal in PromQL, and it stays correct even though the
    // collector's Prometheus exporter runs with `send_timestamps: false`
    // (sample timestamps are scrape-time, not push-time, so a
    // `timestamp()`-based check would not work here).
    let last_run = meter
        .u64_gauge("governance.copilot.last_run_timestamp_seconds")
        .with_description("unix timestamp of the most recent run, by command")
        .build();
    let now = u64::try_from(now_unix).unwrap_or_default();
    last_run.record(now, &[KeyValue::new("command", command.to_owned())]);

    let days_gauge = meter
        .u64_gauge("governance.copilot.days")
        .with_description("report days ingested by the most recent run")
        .build();
    days_gauge.record(days, &[]);

    // A gauge's `record()` overwrites the prior value for an identical
    // attribute set within one collection cycle (last-value-wins), unlike
    // the counter's `add()` it replaces, which summed. A backfill run
    // ingests several days per report type, so pre-aggregating here is what
    // makes the pushed value the actual run total rather than whichever
    // day's outcome happened to be recorded last.
    let mut reports_by_key: HashMap<(String, String), u64> = HashMap::new();
    let mut rows_by_report: HashMap<String, u64> = HashMap::new();
    for o in outcomes {
        *reports_by_key
            .entry((o.report.clone(), o.status.clone()))
            .or_insert(0) += 1;
        *rows_by_report.entry(o.report.clone()).or_insert(0) += o.record_count as u64;
    }

    let reports = meter
        .u64_gauge("governance.copilot.reports")
        .with_description("reports fetched by the most recent run, by report type and outcome")
        .build();
    for ((report, status), count) in reports_by_key {
        reports.record(
            count,
            &[
                KeyValue::new("report", report),
                KeyValue::new("status", status),
            ],
        );
    }

    let rows = meter
        .u64_gauge("governance.copilot.rows")
        .with_description("normalized rows upserted by the most recent run, by report type")
        .build();
    for (report, count) in rows_by_report {
        rows.record(count, &[KeyValue::new("report", report)]);
    }
}

/// Push the run-level gauges (last-run timestamp, reports by status, rows by
/// report, days) -- see `record_run_metrics` for why these are gauges.
pub async fn push_run_metrics(
    endpoint: &str,
    command: &str,
    outcomes: &[governance_copilot::ReportOutcome],
    days: u64,
) {
    let now_unix = chrono::Utc::now().timestamp();
    let res = push(endpoint, |meter| {
        record_run_metrics(meter, command, outcomes, days, now_unix);
    })
    .await;
    if let Err(e) = res {
        warn!(error = %e, "otlp metric push failed; sync result is unaffected");
    }
}

/// What `push_status_metrics` records for each gauge, derived once from a
/// `SyncStatus`. Pulled out as a pure conversion (no OTLP, no async) so the
/// "never synced must not look like age zero" mapping (BLOCKER 3) is
/// directly unit-testable, not only observable through a live metric push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatusRecording {
    ever_synced: u64,
    /// `None` means the age gauge's `.record(...)` is skipped entirely --
    /// an omitted data point, not a fake `0`. Emitting `0` here is exactly
    /// what let a never-synced deployment read as "just succeeded" to an
    /// age-based alert (BLOCKER 3).
    age_seconds: Option<u64>,
    unmapped_users: u64,
}

impl From<SyncStatus> for StatusRecording {
    fn from(status: SyncStatus) -> Self {
        match status {
            SyncStatus::NeverSynced => Self {
                ever_synced: 0,
                age_seconds: None,
                unmapped_users: 0,
            },
            SyncStatus::Synced {
                age_days,
                unmapped_users,
            } => Self {
                ever_synced: 1,
                age_seconds: Some(age_days.max(0) as u64 * 86_400),
                unmapped_users: unmapped_users.max(0) as u64,
            },
        }
    }
}

/// Push the status gauges (whether a sync has ever succeeded, last-success
/// age, unmapped users). See `StatusRecording` for the never-synced/synced
/// mapping this applies.
pub async fn push_status_metrics(endpoint: &str, status: SyncStatus) {
    let recording = StatusRecording::from(status);
    let res = push(endpoint, |meter| {
        let ever_synced = meter
            .u64_gauge("governance.copilot.ever_synced")
            .with_description("1 if any manifest row exists for this tenant, 0 if never synced")
            .build();
        ever_synced.record(recording.ever_synced, &[]);

        if let Some(age_seconds) = recording.age_seconds {
            let age = meter
                .u64_gauge("governance.copilot.last_success_age_seconds")
                .with_description(
                    "seconds since the most recent manifest day; omitted (no data point) \
                     when never synced -- see governance.copilot.ever_synced instead",
                )
                .build();
            age.record(age_seconds, &[]);
        }

        let unmapped_gauge = meter
            .u64_gauge("governance.copilot.unmapped_users")
            .with_description("users with usage but no team row, latest day")
            .build();
        unmapped_gauge.record(recording.unmapped_users, &[]);
    })
    .await;
    if let Err(e) = res {
        warn!(error = %e, "otlp metric push failed; status result is unaffected");
    }
}

/// Push drift count (verify command).
pub async fn push_verify_metrics(endpoint: &str, mismatch: usize) {
    let res = push(endpoint, |meter| {
        let drift = meter
            .u64_gauge("governance.copilot.manifest_drift")
            .with_description("manifest rows whose stored count disagrees")
            .build();
        drift.record(mismatch as u64, &[]);
    })
    .await;
    if let Err(e) = res {
        warn!(error = %e, "otlp metric push failed; verify result is unaffected");
    }
}

/// Record the OTLP day-grain emit outcome (AC 7): the number of log records
/// the collector accepted, by report, and a partial-accept error signal.
///
/// A partial accept -- some records rejected, some accepted -- must be
/// surfaced as an error metric, not swallowed: the usage-side normalizer
/// would otherwise silently miss rows. `rejected > 0` sets the
/// `governance.copilot.emit_partial_accept` gauge to `1`; a fully accepted
/// batch sets it to `0`. Both are gauges (last-value-wins per run), matching
/// the rest of this module's one-shot-push design.
fn record_emit_metrics(meter: &Meter, accepted_by_report: &[(String, u64)], rejected: u64) {
    let accepted = meter
        .u64_gauge("governance.copilot.emit_accepted_rows")
        .with_description("OTLP log records the collector accepted, by report")
        .build();

    // A gauge's `record()` overwrites the prior value for an identical
    // attribute set within one collection cycle (last-value-wins). A backfill
    // run emits several days per report type, so `accepted_by_report` carries
    // one entry per export -- several entries sharing a `report` label.
    // Recording each entry directly would silently drop every day but the
    // last (the gauge-collapse defect). Sum per report so the pushed value is
    // the run total, not whichever day's count happened to be recorded last.
    let mut totals: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for (report, count) in accepted_by_report {
        *totals.entry(report.clone()).or_insert(0) += *count;
    }
    for (report, count) in totals {
        accepted.record(count, &[KeyValue::new("report", report)]);
    }

    let partial = meter
        .u64_gauge("governance.copilot.emit_partial_accept")
        .with_description("1 if any OTLP record was rejected (partial accept), else 0")
        .build();
    partial.record(u64::from(rejected > 0), &[]);
}

/// Push the OTLP emit outcome gauges (see `record_emit_metrics`).
pub async fn push_emit_metrics(
    endpoint: &str,
    accepted_by_report: &[(String, u64)],
    rejected: u64,
) {
    let res = push(endpoint, |meter| {
        record_emit_metrics(meter, accepted_by_report, rejected);
    })
    .await;
    if let Err(e) = res {
        warn!(error = %e, "otlp emit metric push failed; emit result is unaffected");
    }
}

/// Build a one-shot meter provider, record `record`, and force-flush before
/// the process exits (the CronJob pod dies immediately after main returns).
async fn push(endpoint: &str, record: impl FnOnce(&Meter)) -> Result<()> {
    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()
        .map_err(|e| anyhow::anyhow!("otlp exporter: {e}"))?;
    let reader = opentelemetry_sdk::metrics::PeriodicReader::builder(exporter).build();
    let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
        .with_reader(reader)
        .build();
    record(&provider.meter("governance_copilot"));
    provider
        .force_flush()
        .map_err(|e| anyhow::anyhow!("otlp flush: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use opentelemetry_sdk::metrics::{
        InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
        data::{AggregatedMetrics, MetricData, ResourceMetrics},
    };

    use super::*;

    /// Drives `record` against an `SdkMeterProvider` backed by an in-memory
    /// exporter (no network) and hands back every exported `ResourceMetrics`
    /// for inspection. This exercises the exact `meter.u64_gauge(...)` /
    /// `meter.u64_counter(...)` calls `record_run_metrics` makes, so a test
    /// built on this sees the real instrument type the SDK assigned -- not
    /// just a value that happens to look right.
    fn export(record: impl FnOnce(&Meter)) -> Vec<ResourceMetrics> {
        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter.clone()).build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        record(&provider.meter("governance_copilot_test"));
        provider
            .force_flush()
            .expect("flush of an in-memory exporter cannot fail");
        exporter
            .get_finished_metrics()
            .expect("in-memory exporter storage cannot fail to be read back")
    }

    fn u64_gauge_points(
        resource_metrics: &[ResourceMetrics],
        name: &str,
    ) -> Vec<(Vec<(String, String)>, u64)> {
        resource_metrics
            .iter()
            .flat_map(ResourceMetrics::scope_metrics)
            .flat_map(|sm| sm.metrics())
            .find(|m| m.name() == name)
            .map_or_else(
                || panic!("metric {name} was not recorded at all"),
                |m| match m.data() {
                    AggregatedMetrics::U64(MetricData::Gauge(gauge)) => gauge
                        .data_points()
                        .map(|p| {
                            let labels = p
                                .attributes()
                                .map(|kv| (kv.key.as_str().to_owned(), kv.value.to_string()))
                                .collect();
                            (labels, p.value())
                        })
                        .collect(),
                    other => panic!(
                        "expected {name} to be a u64 Gauge, got {other:?} -- a Sum/Counter here \
                         is the exact bug this fix closes: rebuilt every run with no surviving \
                         state, it either stays permanently 1 (governance.copilot.run) or \
                         bounces nonsensically up and down depending on how much a single run \
                         covered"
                    ),
                },
            )
    }

    fn outcome(
        report: &str,
        status: &str,
        record_count: usize,
    ) -> governance_copilot::ReportOutcome {
        governance_copilot::ReportOutcome {
            report: report.to_owned(),
            day: "2026-08-05".to_owned(),
            status: status.to_owned(),
            record_count,
            host: None,
        }
    }

    /// The core assertion the review demanded: `governance.copilot.
    /// last_run_timestamp_seconds` (the `governance.copilot.run` counter's
    /// replacement) must be a Gauge carrying the actual unix timestamp, not
    /// a Sum. Confirmed against the pre-fix code (see report): building this
    /// instrument with `meter.u64_counter(...)` instead makes
    /// `u64_gauge_points` panic with "expected ... to be a u64 Gauge, got
    /// U64(Sum(...))", because a Counter is exported as `AggregatedMetrics::
    /// U64(MetricData::Sum(..))`, not `Gauge`.
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

    /// A backfill run ingests several days per report type. Because a
    /// gauge's `record()` overwrites (last-value-wins) rather than sums for
    /// an identical attribute set within one collection cycle, recording
    /// each day's outcome directly (the naive counter->gauge swap) would
    /// silently drop every day but the last. This proves the pre-aggregation
    /// in `record_run_metrics` actually happened: two days of the same
    /// report/status must sum to a `reports` value of 2 and a `rows` value
    /// of 3 + 5 = 8, not whichever day was recorded last.
    ///
    /// Confirmed against a deliberately un-aggregated version (recording
    /// straight from the `outcomes` loop instead of `reports_by_key`/
    /// `rows_by_report`): it failed with `reports` = 1 and `rows` = 5 (the
    /// second day's values only), exactly the silent-data-loss mechanism
    /// this test exists to catch.
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
    /// carry it -- a dashboard author reading the name alone must not be
    /// misled into reaching for `rate()`/`increase()`.
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

    /// BLOCKER 3, the core assertion: a never-synced deployment must not
    /// compute the same age as one that just succeeded. Before this fix,
    /// `run_status` returned the sentinel `(-1, -1)` and `push_status_
    /// metrics` did `age_days.max(0) as u64 * 86_400`, which folds `-1`
    /// into `0` -- identical to `SyncStatus::Synced { age_days: 0, .. }`.
    #[test]
    fn never_synced_does_not_compute_the_same_recording_as_synced_zero_days_ago() {
        let never = StatusRecording::from(SyncStatus::NeverSynced);
        let just_succeeded = StatusRecording::from(SyncStatus::Synced {
            age_days: 0,
            unmapped_users: 0,
        });
        assert_ne!(never, just_succeeded);
    }

    /// The age gauge must be omitted (no data point), not a fake zero --
    /// that is the entire fix, stated as directly as possible.
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

    /// A negative age (clock skew, or a report_day briefly in the future)
    /// must clamp to zero, not underflow the `u64` cast.
    #[test]
    fn synced_clamps_a_negative_age_to_zero() {
        let recording = StatusRecording::from(SyncStatus::Synced {
            age_days: -1,
            unmapped_users: 0,
        });
        assert_eq!(recording.age_seconds, Some(0));
    }

    /// AC 7: a partial accept (some records rejected) must surface as an
    /// error metric (`emit_partial_accept = 1`), not be swallowed. A fully
    /// accepted batch must read `0`.
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

    /// The `report` attribute of a gauge point, for order-independent
    /// comparison.
    fn report_label(p: &(Vec<(String, String)>, u64)) -> &str {
        p.0.iter()
            .find(|(k, _)| k == "report")
            .map_or("", |(_, v)| v.as_str())
    }

    /// Sort gauge points by their `report` attribute so an assertion is
    /// order-independent -- the SDK stores data points in a HashMap, so
    /// iteration order is not guaranteed.
    fn sort_by_report(points: &mut [(Vec<(String, String)>, u64)]) {
        points.sort_by(|a, b| report_label(a).cmp(report_label(b)));
    }

    /// AC 7: `emit_accepted_rows` reflects the records the collector
    /// accepted, by report.
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
        let mut points =
            u64_gauge_points(&resource_metrics, "governance.copilot.emit_accepted_rows");
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

    /// The gauge-collapse regression (adversarial review): a backfill run
    /// emits several days per report type, so `accepted_by_report` carries
    /// several entries sharing a `report` label. A gauge's `record()`
    /// overwrites (last-value-wins) for an identical attribute set, so
    /// recording each entry directly would report only the final day's count
    /// instead of the run total. This proves the per-report pre-aggregation
    /// in `record_emit_metrics` happened: two days of the same report must
    /// sum to 3 + 5 = 8, not whichever day was recorded last.
    ///
    /// Confirmed against a deliberately un-aggregated version (recording
    /// straight from the `accepted_by_report` loop): it failed with a single
    /// point of value 5 (the second day only), exactly the silent-undercount
    /// this test exists to catch.
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
}
