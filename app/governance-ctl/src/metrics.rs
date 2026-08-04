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
use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider;
use opentelemetry_otlp::WithExportConfig;
use tracing::warn;

/// Endpoint env var (standard OTel naming); absent = push skipped.
pub fn endpoint_from_env() -> Option<String> {
    std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .ok()
        .filter(|e| !e.is_empty())
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

/// Push the status gauges (last-success age, unmapped users).
pub async fn push_status_metrics(endpoint: &str, age_days: i64, unmapped: i64) {
    let res = push(endpoint, |meter| {
        let age = meter
            .u64_gauge("governance.copilot.last_success_age_seconds")
            .with_description("seconds since the most recent manifest day")
            .build();
        age.record(age_days.max(0) as u64 * 86_400, &[]);

        let unmapped_gauge = meter
            .u64_gauge("governance.copilot.unmapped_users")
            .with_description("users with usage but no team row, latest day")
            .build();
        unmapped_gauge.record(unmapped.max(0) as u64, &[]);
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
