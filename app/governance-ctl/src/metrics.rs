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

use anyhow::Result;
use opentelemetry::{KeyValue, metrics::MeterProvider};
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

/// Push the run-level counters (reports by status, rows by report, days).
pub async fn push_run_metrics(
    endpoint: &str,
    command: &str,
    outcomes: &[governance_copilot::ReportOutcome],
    days: u64,
) {
    let res = push(endpoint, |meter| {
        let run = meter
            .u64_counter("governance.copilot.run")
            .with_description("collector runs, by command")
            .build();
        run.add(1, &[KeyValue::new("command", command.to_owned())]);

        let days_counter = meter
            .u64_counter("governance.copilot.days")
            .with_description("report days ingested by this run")
            .build();
        days_counter.add(days, &[]);

        let reports = meter
            .u64_counter("governance.copilot.reports")
            .with_description("reports fetched, by report type and outcome")
            .build();
        let rows = meter
            .u64_counter("governance.copilot.rows")
            .with_description("normalized rows upserted, by report type")
            .build();
        for o in outcomes {
            reports.add(
                1,
                &[
                    KeyValue::new("report", o.report.clone()),
                    KeyValue::new("status", o.status.clone()),
                ],
            );
            rows.add(
                o.record_count as u64,
                &[KeyValue::new("report", o.report.clone())],
            );
        }
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

/// Build a one-shot meter provider, record `record`, and force-flush before
/// the process exits (the CronJob pod dies immediately after main returns).
async fn push(endpoint: &str, record: impl FnOnce(&opentelemetry::metrics::Meter)) -> Result<()> {
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
    use super::*;

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
}
