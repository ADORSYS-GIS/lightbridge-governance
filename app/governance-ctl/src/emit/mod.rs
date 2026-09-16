//! OTLP day-grain emission (ADR-0014 Decision 2 / RFC-0001 encoding contract).
//!
//! `governance-ctl` emits day-grain facts and seat snapshots as **OTLP log
//! records** through the authenticated edge OTEL collector, where the
//! usage-side day-grain normalizer in `lightbridge-authz` reads them into the
//! generalized `usage_day_facts` / `usage_seat_snapshots` tables.
//!
//! The encoding is a **contract** with that normalizer (RFC-0001 "OTLP
//! day-grain encoding contract"): one log record per (report, subject), typed
//! attributes, money as integer micro-USD (ADR-0008). This module is the
//! governance-side half of that contract.
//!
//! Encoding is split from emission so it is unit-testable without a network:
//! [`encode::LogRecordData`] is a pure, transport-agnostic representation
//! (body + typed attributes); [`emit`] turns those into OTLP log records over
//! the collector. The round-trip tests assert the encoding matches the
//! documented contract exactly.

mod encode;

use anyhow::Result;
pub use encode::{
    AttributeValue, LogRecordData, encode_org_daily, encode_repo_daily, encode_seat,
    encode_user_daily, encode_user_team,
};
use opentelemetry::logs::{AnyValue, LogRecord, Logger, LoggerProvider};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::logs::{BatchLogProcessor, SdkLoggerProvider};

/// The OTLP day-grain sink: encodes normalized rows and emits them as OTLP
/// log records through the authenticated edge collector.
///
/// Config-gated -- constructed from `OTEL_EXPORTER_OTLP_ENDPOINT` (the same
/// env var `metrics.rs` uses for the operational gauges). The direct-Postgres
/// write path stays active until the cutover (lightbridge-authz#588); this
/// sink is the ADR-0014 replacement, ready to be switched over.
///
/// The sink tracks per-report accepted counts and a rejected count (AC 7) so
/// the caller can surface a partial accept as an error metric rather than
/// swallowing it. `emit_rows` records a report's records as accepted only if
/// the export succeeds; a failed export counts them as rejected.
#[derive(Clone)]
pub struct Sink {
    endpoint: String,
    stats: std::sync::Arc<std::sync::Mutex<EmitStats>>,
}

#[derive(Default)]
struct EmitStats {
    accepted_by_report: Vec<(String, u64)>,
    rejected: u64,
}

impl Sink {
    /// Build from `OTEL_EXPORTER_OTLP_ENDPOINT`. `None` when unset/empty --
    /// the sink is then simply not used (the Postgres path remains).
    pub fn from_env() -> Option<Sink> {
        let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
            .ok()
            .filter(|e| !e.is_empty())?;
        Some(Sink {
            endpoint,
            stats: std::sync::Arc::default(),
        })
    }

    /// Accepted records by report, and the total rejected count, since this
    /// sink was built (AC 7). A partial accept is `rejected > 0`.
    pub fn stats(&self) -> (Vec<(String, u64)>, u64) {
        let stats = self.stats.lock().unwrap_or_else(|e| e.into_inner());
        (stats.accepted_by_report.clone(), stats.rejected)
    }

    /// Encode `rows` and emit them as OTLP log records. Returns the number of
    /// records emitted. A failed export surfaces as `Err` (the caller decides
    /// whether to treat it as a run failure); it is never swallowed here, and
    /// the records are counted as rejected for the AC 7 metric.
    pub async fn emit_rows(
        &self,
        tenant_id: &str,
        org: &str,
        rows: &governance_copilot::ParsedRows,
    ) -> Result<usize> {
        let (report, records): (String, Vec<LogRecordData>) = match rows {
            governance_copilot::ParsedRows::Org(rs) => (
                "organization-1-day".to_owned(),
                rs.iter()
                    .map(|r| encode_org_daily(tenant_id, org, r))
                    .collect(),
            ),
            governance_copilot::ParsedRows::User(rs) => (
                "users-1-day".to_owned(),
                rs.iter()
                    .map(|r| encode_user_daily(tenant_id, org, r))
                    .collect(),
            ),
            governance_copilot::ParsedRows::Repo(rs) => (
                "repos-1-day".to_owned(),
                rs.iter()
                    .map(|r| encode_repo_daily(tenant_id, org, r))
                    .collect(),
            ),
            governance_copilot::ParsedRows::UserTeam(rs) => (
                "user-teams-1-day".to_owned(),
                rs.iter()
                    .map(|r| encode_user_team(tenant_id, org, r))
                    .collect(),
            ),
            governance_copilot::ParsedRows::Seat(rs) => (
                "billing-seats".to_owned(),
                rs.iter().map(|r| encode_seat(tenant_id, org, r)).collect(),
            ),
        };
        let n = records.len() as u64;
        match emit(&records, &self.endpoint).await {
            Ok(()) => {
                self.stats
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .accepted_by_report
                    .push((report, n));
                Ok(n as usize)
            }
            Err(e) => {
                self.stats
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .rejected += n;
                Err(e)
            }
        }
    }
}

/// Emit `records` as OTLP log records through the collector at `endpoint`.
///
/// A failed push is surfaced to the caller (the run decides whether to treat
/// it as a failure); it is not swallowed here. The caller is responsible for
/// the partial-accept accounting (AC 7) -- this function reports the raw
/// export result.
pub async fn emit(records: &[LogRecordData], endpoint: &str) -> Result<()> {
    let exporter = opentelemetry_otlp::LogExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()
        .map_err(|e| anyhow::anyhow!("otlp log exporter: {e}"))?;
    let processor = BatchLogProcessor::builder(exporter).build();
    let provider = SdkLoggerProvider::builder()
        .with_log_processor(processor)
        .build();
    let logger = provider.logger("governance_copilot");

    for r in records {
        let mut record = logger.create_log_record();
        record.set_body(AnyValue::from(r.body.clone()));
        record.set_severity_text("INFO");
        for (k, v) in &r.attributes {
            match v {
                AttributeValue::Str(s) => record.add_attribute(k.clone(), s.clone()),
                AttributeValue::Int(i) => record.add_attribute(k.clone(), *i),
            }
        }
        logger.emit(record);
    }

    provider
        .force_flush()
        .map_err(|e| anyhow::anyhow!("otlp log flush: {e}"))?;
    Ok(())
}
