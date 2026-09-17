//! OTLP day-grain emission (ADR-0014 Decision 2 / RFC-0001 encoding contract).
//!
//! `governance-ctl` emits day-grain facts and seat snapshots as **OTLP log
//! records** through the authenticated edge OTEL collector, where the
//! usage-side day-grain normalizer in `lightbridge-authz` reads them into the
//! generalized `usage_day_facts` / `usage_seat_snapshots` tables. The encoding
//! is a **contract** with that normalizer: one log record per (report,
//! subject), typed attributes, money as integer micro-USD (ADR-0008).
//!
//! Encoding is split from emission so it is unit-testable without a network:
//! [`encode::LogRecordData`] is a pure, transport-agnostic representation;
//! [`Sink`] turns those into OTLP log records over the collector. The pure
//! helpers (per-report encoding dispatch, org-cost aggregation, collision
//! guard) live in `helpers`.
//!
//! # Accounting semantics (AC 7)
//!
//! [`Sink::emit_rows`] counts a batch's records as **accepted** only when the
//! underlying OTLP export succeeds, and as **rejected** when it fails. This is
//! a *batch-level* signal: the OpenTelemetry SDK does not expose per-record
//! accept/reject through the high-level log API, so a partial accept within a
//! single export is indistinguishable from a full accept at this layer -- the
//! caller should treat a non-zero `rejected` as the hard signal.

mod encode;
mod helpers;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use encode::AttributeValue;
use governance_copilot::ParsedRows;
use helpers::{aggregate_org_cost, detect_collisions, encode_report};
use opentelemetry::logs::{AnyValue, LogRecord, Logger, LoggerProvider};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::logs::{BatchLogProcessor, SdkLogger, SdkLoggerProvider};

/// The OTLP day-grain sink: encodes normalized rows and emits them as OTLP
/// log records through the authenticated edge collector.
///
/// Config-gated -- constructed from `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`
/// (falling back to `OTEL_EXPORTER_OTLP_ENDPOINT`). The direct-Postgres write
/// path stays active until the cutover (lightbridge-authz#588); this sink is
/// the ADR-0014 replacement, ready to be switched over.
///
/// The sink owns a single [`SdkLoggerProvider`] built once at construction and
/// reuses it across every `emit_rows` call (rebuilding an exporter/provider
/// per call would open a fresh gRPC connection for each). The provider is
/// force-flushed after each batch so a failed export surfaces to the caller
/// and is counted as rejected (AC 7).
#[derive(Clone)]
pub struct Sink {
    logger: SdkLogger,
    provider: SdkLoggerProvider,
    stats: Arc<Mutex<EmitStats>>,
}

#[derive(Default)]
struct EmitStats {
    accepted_by_report: Vec<(String, u64)>,
    rejected: u64,
}

impl Sink {
    /// Build from `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`, falling back to
    /// `OTEL_EXPORTER_OTLP_ENDPOINT`. `Ok(None)` when neither is set/empty
    /// (the Postgres path remains); a configured-but-unbuildable endpoint is
    /// an `Err` (fail loudly rather than silently disable the write path).
    pub fn from_env() -> Result<Option<Sink>> {
        let endpoint = std::env::var("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT")
            .ok()
            .filter(|e| !e.is_empty())
            .or_else(|| {
                std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                    .ok()
                    .filter(|e| !e.is_empty())
            });
        let Some(endpoint) = endpoint else {
            return Ok(None);
        };
        Ok(Some(Self::new(endpoint)?))
    }

    /// Build a sink over a real OTLP endpoint; the exporter/provider are
    /// constructed once here and reused for the lifetime of the sink.
    fn new(endpoint: String) -> Result<Sink> {
        let exporter = opentelemetry_otlp::LogExporter::builder()
            .with_tonic()
            .with_endpoint(&endpoint)
            .build()
            .map_err(|e| anyhow!("otlp log exporter: {e}"))?;
        let processor = BatchLogProcessor::builder(exporter).build();
        let provider = SdkLoggerProvider::builder()
            .with_log_processor(processor)
            .build();
        let logger = provider.logger("governance_copilot");
        Ok(Sink {
            logger,
            provider,
            stats: Arc::default(),
        })
    }

    /// Build a sink over an injected provider (tests use an in-memory
    /// exporter). Test-only: unreachable from any production build.
    #[cfg(test)]
    fn from_provider(provider: SdkLoggerProvider) -> Sink {
        let logger = provider.logger("governance_copilot_test");
        Sink {
            logger,
            provider,
            stats: Arc::default(),
        }
    }

    /// Accepted records by report, and the total rejected count, since this
    /// sink was built (AC 7). A partial accept is `rejected > 0`.
    pub fn stats(&self) -> (Vec<(String, u64)>, u64) {
        let stats = self.stats.lock().unwrap_or_else(|e| e.into_inner());
        (stats.accepted_by_report.clone(), stats.rejected)
    }

    /// Encode `rows` (all report kinds for one day) and emit them as OTLP log
    /// records. Returns the number of records emitted. A failed export
    /// surfaces as `Err` (never swallowed) and the records are counted as
    /// rejected for the AC 7 metric.
    ///
    /// `strict` is the cutover switch (`cfg.freeze_writes`): a natural-key
    /// collision within the batch (RFC-0001 known-issues #1/#5) fails the emit
    /// loudly when `true`, and is logged as a warning when `false` (shadow
    /// mode, Postgres still authoritative).
    pub async fn emit_rows(
        &self,
        tenant_id: &str,
        org: &str,
        rows: &[ParsedRows],
        strict: bool,
    ) -> Result<usize> {
        // The org report carries no cost (GitHub reports credits/cost per-user
        // only), so the org-level spend is aggregated from the day's user rows.
        let (org_credits, org_cost) = aggregate_org_cost(rows);

        let mut records = Vec::new();
        let mut per_report: Vec<(String, u64)> = Vec::new();
        for parsed in rows {
            // `user-teams-1-day` is not cut over: the authz-side receiver
            // refuses it (RFC-0001 known-issue #1), so it is skipped here.
            if matches!(parsed, ParsedRows::UserTeam(_)) {
                tracing::warn!(
                    "user-teams-1-day is not cut over (RFC-0001 known-issue #1); \
                     the authz-side receiver refuses it, skipping its OTLP emission"
                );
                continue;
            }
            let (report, recs) = encode_report(tenant_id, org, parsed, org_credits, org_cost)?;
            per_report.push((report, recs.len() as u64));
            records.extend(recs);
        }

        // Refuse (or warn) on natural-key collisions so a lost record is loud.
        detect_collisions(&records, strict)?;

        let total = records.len() as u64;
        for r in &records {
            let mut record = self.logger.create_log_record();
            record.set_body(AnyValue::from(r.body.clone()));
            record.set_severity_text("INFO");
            for (k, v) in &r.attributes {
                match v {
                    AttributeValue::Str(s) => record.add_attribute(k.clone(), s.clone()),
                    AttributeValue::Int(i) => record.add_attribute(k.clone(), *i),
                }
            }
            self.logger.emit(record);
        }

        match self.provider.force_flush() {
            Ok(()) => {
                self.stats
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .accepted_by_report
                    .extend(per_report);
                Ok(total as usize)
            }
            Err(e) => {
                self.stats
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .rejected += total;
                Err(anyhow!("otlp log flush: {e}"))
            }
        }
    }
}
