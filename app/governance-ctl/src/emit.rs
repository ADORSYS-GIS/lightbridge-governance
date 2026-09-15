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
//! [`LogRecordData`] is a pure, transport-agnostic representation (body +
//! typed attributes); [`emit`] turns those into OTLP log records over the
//! collector. The round-trip tests in this module assert the encoding matches
//! the documented contract exactly.

use anyhow::Result;
use governance_copilot::{OrgDaily, RepoDaily, SeatSnapshot, UserDaily, UserTeam};
use opentelemetry::logs::{AnyValue, LogRecord, Logger, LoggerProvider};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::logs::{BatchLogProcessor, SdkLoggerProvider};

/// The trusted-source stamp carried on every record (ADR-0013 invariant 2 /
/// RFC-0001 contract). The usage-side normalizer keys on this.
pub const SOURCE: &str = "github-copilot";

/// A pure, transport-agnostic log record: a human-readable body plus typed
/// attributes. Encoding produces these; [`emit`] turns them into OTLP log
/// records. Kept separate so the encoding is testable without any network or
/// OTLP SDK machinery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecordData {
    pub body: String,
    pub attributes: Vec<(String, AttributeValue)>,
}

/// A typed attribute value. Only the two types the contract pins appear:
/// strings and integers (money and counts are integers; never floats).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeValue {
    Str(String),
    Int(i64),
}

/// The seven common attributes present on every record (RFC-0001 contract).
fn common(
    tenant_id: &str,
    org: &str,
    report: &str,
    day: &str,
    subject_kind: &str,
    subject_id: &str,
) -> Vec<(String, AttributeValue)> {
    vec![
        ("source".to_owned(), AttributeValue::Str(SOURCE.to_owned())),
        (
            "tenant_id".to_owned(),
            AttributeValue::Str(tenant_id.to_owned()),
        ),
        ("org".to_owned(), AttributeValue::Str(org.to_owned())),
        ("report".to_owned(), AttributeValue::Str(report.to_owned())),
        ("day".to_owned(), AttributeValue::Str(day.to_owned())),
        (
            "subject_kind".to_owned(),
            AttributeValue::Str(subject_kind.to_owned()),
        ),
        (
            "subject_id".to_owned(),
            AttributeValue::Str(subject_id.to_owned()),
        ),
    ]
}

/// Encode one `organization-1-day` row.
pub fn encode_org_daily(tenant_id: &str, org: &str, row: &OrgDaily) -> LogRecordData {
    let mut attrs = common(
        tenant_id,
        org,
        "organization-1-day",
        &row.report_day,
        "org",
        &row.organization_id,
    );
    attrs.push((
        "active_users".to_owned(),
        AttributeValue::Int(row.active_users as i64),
    ));
    attrs.push((
        "engaged_users".to_owned(),
        AttributeValue::Int(row.engaged_users as i64),
    ));
    attrs.push((
        "total_interactions".to_owned(),
        AttributeValue::Int(row.total_interactions as i64),
    ));
    attrs.push((
        "total_completions".to_owned(),
        AttributeValue::Int(row.total_completions as i64),
    ));
    attrs.push((
        "ai_credits".to_owned(),
        AttributeValue::Int(row.ai_credits as i64),
    ));
    attrs.push((
        "net_cost_micro_usd".to_owned(),
        AttributeValue::Int(row.net_cost_micro_usd.0),
    ));
    LogRecordData {
        body: format!(
            "org {} {}: {} active, {} engaged, {} interactions",
            row.organization_id,
            row.report_day,
            row.active_users,
            row.engaged_users,
            row.total_interactions
        ),
        attributes: attrs,
    }
}

/// Encode one `users-1-day` row.
pub fn encode_user_daily(tenant_id: &str, org: &str, row: &UserDaily) -> LogRecordData {
    let mut attrs = common(
        tenant_id,
        org,
        "users-1-day",
        &row.report_day,
        "user",
        &row.provider_user_id,
    );
    attrs.push((
        "user_login".to_owned(),
        AttributeValue::Str(row.user_login.clone()),
    ));
    attrs.push((
        "total_interactions".to_owned(),
        AttributeValue::Int(row.total_interactions as i64),
    ));
    attrs.push((
        "total_completions".to_owned(),
        AttributeValue::Int(row.total_completions as i64),
    ));
    attrs.push((
        "ai_credits".to_owned(),
        AttributeValue::Int(row.ai_credits as i64),
    ));
    attrs.push((
        "net_cost_micro_usd".to_owned(),
        AttributeValue::Int(row.net_cost_micro_usd.0),
    ));
    LogRecordData {
        body: format!(
            "user {} {}: {} interactions, {} completions",
            row.provider_user_id, row.report_day, row.total_interactions, row.total_completions
        ),
        attributes: attrs,
    }
}

/// Encode one `repos-1-day` row.
pub fn encode_repo_daily(tenant_id: &str, org: &str, row: &RepoDaily) -> LogRecordData {
    let mut attrs = common(
        tenant_id,
        org,
        "repos-1-day",
        &row.report_day,
        "repo",
        &row.repository_id,
    );
    attrs.push((
        "coding_agent_activity".to_owned(),
        AttributeValue::Int(row.coding_agent_activity as i64),
    ));
    attrs.push((
        "code_review_activity".to_owned(),
        AttributeValue::Int(row.code_review_activity as i64),
    ));
    attrs.push((
        "pull_request_activity".to_owned(),
        AttributeValue::Int(row.pull_request_activity as i64),
    ));
    LogRecordData {
        body: format!(
            "repo {} {}: {} coding, {} review, {} pr",
            row.repository_id,
            row.report_day,
            row.coding_agent_activity,
            row.code_review_activity,
            row.pull_request_activity
        ),
        attributes: attrs,
    }
}

/// Encode one `user-teams-1-day` row.
pub fn encode_user_team(tenant_id: &str, org: &str, row: &UserTeam) -> LogRecordData {
    let mut attrs = common(
        tenant_id,
        org,
        "user-teams-1-day",
        &row.report_day,
        "user_team",
        &row.user_id,
    );
    attrs.push((
        "team_id".to_owned(),
        AttributeValue::Str(row.team_id.clone()),
    ));
    attrs.push((
        "team_slug".to_owned(),
        AttributeValue::Str(row.team_slug.clone()),
    ));
    LogRecordData {
        body: format!(
            "user {} -> team {} ({}) {}",
            row.user_id, row.team_id, row.team_slug, row.report_day
        ),
        attributes: attrs,
    }
}

/// Encode one `billing-seats` row (a seat snapshot).
pub fn encode_seat(tenant_id: &str, org: &str, row: &SeatSnapshot) -> LogRecordData {
    let mut attrs = common(
        tenant_id,
        org,
        "billing-seats",
        &row.snapshot_day,
        "seat",
        &row.provider_user_id,
    );
    attrs.push((
        "user_login".to_owned(),
        AttributeValue::Str(row.user_login.clone()),
    ));
    if let Some(t) = &row.seat_assigned_at {
        attrs.push((
            "seat_assigned_at".to_owned(),
            AttributeValue::Str(t.to_rfc3339()),
        ));
    }
    if let Some(t) = &row.last_activity_at {
        attrs.push((
            "last_activity_at".to_owned(),
            AttributeValue::Str(t.to_rfc3339()),
        ));
    }
    if let Some(e) = &row.last_activity_editor {
        attrs.push((
            "last_activity_editor".to_owned(),
            AttributeValue::Str(e.clone()),
        ));
    }
    attrs.push((
        "seat_state".to_owned(),
        AttributeValue::Str(row.seat_state.clone()),
    ));
    LogRecordData {
        body: format!(
            "seat {} ({}) {} {}",
            row.provider_user_id, row.user_login, row.seat_state, row.snapshot_day
        ),
        attributes: attrs,
    }
}

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

#[cfg(test)]
mod tests {
    use governance_core::MicroUsd;

    use super::*;

    fn attr<'a>(attrs: &'a [(String, AttributeValue)], key: &str) -> &'a AttributeValue {
        attrs
            .iter()
            .find(|(k, _)| k == key)
            .map_or_else(|| panic!("attribute {key} missing"), |(_, v)| v)
    }

    fn int(attrs: &[(String, AttributeValue)], key: &str) -> i64 {
        match attr(attrs, key) {
            AttributeValue::Int(i) => *i,
            other => panic!("attribute {key} expected Int, got {other:?}"),
        }
    }

    fn str_attr<'a>(attrs: &'a [(String, AttributeValue)], key: &str) -> &'a str {
        match attr(attrs, key) {
            AttributeValue::Str(s) => s,
            other => panic!("attribute {key} expected Str, got {other:?}"),
        }
    }

    /// The common attributes must be present and pinned on every record kind
    /// (RFC-0001 contract): source, tenant_id, org, report, day, subject_kind,
    /// subject_id.
    #[test]
    fn common_attributes_are_pinned_on_every_record() {
        let row = OrgDaily {
            organization_id: "g1".to_owned(),
            report_day: "2026-08-01".to_owned(),
            active_users: 10,
            engaged_users: 4,
            total_interactions: 150,
            total_completions: 120,
            ai_credits: 0,
            net_cost_micro_usd: MicroUsd(0),
        };
        let rec = encode_org_daily("t1", "g1", &row);
        assert_eq!(str_attr(&rec.attributes, "source"), "github_copilot");
        assert_eq!(str_attr(&rec.attributes, "tenant_id"), "t1");
        assert_eq!(str_attr(&rec.attributes, "org"), "g1");
        assert_eq!(str_attr(&rec.attributes, "report"), "organization-1-day");
        assert_eq!(str_attr(&rec.attributes, "day"), "2026-08-01");
        assert_eq!(str_attr(&rec.attributes, "subject_kind"), "org");
        assert_eq!(str_attr(&rec.attributes, "subject_id"), "g1");
    }

    /// `organization-1-day` worked example from the RFC-0001 contract.
    #[test]
    fn org_daily_matches_the_contract_worked_example() {
        let row = OrgDaily {
            organization_id: "g1".to_owned(),
            report_day: "2026-08-01".to_owned(),
            active_users: 10,
            engaged_users: 4,
            total_interactions: 150,
            total_completions: 120,
            ai_credits: 0,
            net_cost_micro_usd: MicroUsd(0),
        };
        let rec = encode_org_daily("t1", "g1", &row);
        assert_eq!(int(&rec.attributes, "active_users"), 10);
        assert_eq!(int(&rec.attributes, "engaged_users"), 4);
        assert_eq!(int(&rec.attributes, "total_interactions"), 150);
        assert_eq!(int(&rec.attributes, "total_completions"), 120);
        assert_eq!(int(&rec.attributes, "ai_credits"), 0);
        assert_eq!(int(&rec.attributes, "net_cost_micro_usd"), 0);
    }

    /// `users-1-day` worked example from the RFC-0001 contract: money is
    /// integer micro-USD (25000 for 2.5 credits), never a float.
    #[test]
    fn user_daily_matches_the_contract_worked_example() {
        let row = UserDaily {
            provider_user_id: "1001".to_owned(),
            user_login: "octocat".to_owned(),
            report_day: "2026-08-01".to_owned(),
            total_interactions: 42,
            total_completions: 20,
            ai_credits: 2,
            net_cost_micro_usd: MicroUsd(25_000),
        };
        let rec = encode_user_daily("t1", "g1", &row);
        assert_eq!(str_attr(&rec.attributes, "subject_kind"), "user");
        assert_eq!(str_attr(&rec.attributes, "subject_id"), "1001");
        assert_eq!(str_attr(&rec.attributes, "user_login"), "octocat");
        assert_eq!(int(&rec.attributes, "total_interactions"), 42);
        assert_eq!(int(&rec.attributes, "total_completions"), 20);
        assert_eq!(int(&rec.attributes, "ai_credits"), 2);
        assert_eq!(int(&rec.attributes, "net_cost_micro_usd"), 25_000);
    }

    /// `repos-1-day` worked example from the RFC-0001 contract.
    #[test]
    fn repo_daily_matches_the_contract_worked_example() {
        let row = RepoDaily {
            repository_id: "844522530".to_owned(),
            report_day: "2026-08-01".to_owned(),
            coding_agent_activity: 3,
            code_review_activity: 1,
            pull_request_activity: 2,
        };
        let rec = encode_repo_daily("t1", "g1", &row);
        assert_eq!(str_attr(&rec.attributes, "subject_kind"), "repo");
        assert_eq!(str_attr(&rec.attributes, "subject_id"), "844522530");
        assert_eq!(int(&rec.attributes, "coding_agent_activity"), 3);
        assert_eq!(int(&rec.attributes, "code_review_activity"), 1);
        assert_eq!(int(&rec.attributes, "pull_request_activity"), 2);
    }

    /// `user-teams-1-day` worked example from the RFC-0001 contract.
    #[test]
    fn user_team_matches_the_contract_worked_example() {
        let row = UserTeam {
            user_id: "1001".to_owned(),
            team_id: "9001".to_owned(),
            team_slug: "eng-platform".to_owned(),
            report_day: "2026-08-01".to_owned(),
        };
        let rec = encode_user_team("t1", "g1", &row);
        assert_eq!(str_attr(&rec.attributes, "subject_kind"), "user_team");
        assert_eq!(str_attr(&rec.attributes, "subject_id"), "1001");
        assert_eq!(str_attr(&rec.attributes, "team_id"), "9001");
        assert_eq!(str_attr(&rec.attributes, "team_slug"), "eng-platform");
    }

    /// `billing-seats` worked example from the RFC-0001 contract, including
    /// the optional timestamp/editor attributes.
    #[test]
    fn seat_matches_the_contract_worked_example() {
        let row = SeatSnapshot {
            provider_user_id: "1001".to_owned(),
            user_login: "octocat".to_owned(),
            snapshot_day: "2026-08-07".to_owned(),
            seat_assigned_at: Some("2026-01-01T00:00:00Z".parse().unwrap()),
            last_activity_at: Some("2026-08-01T09:30:00Z".parse().unwrap()),
            last_activity_editor: Some("vscode/1.90.0/copilot/1.200.0".to_owned()),
            seat_state: "active".to_owned(),
        };
        let rec = encode_seat("t1", "g1", &row);
        assert_eq!(str_attr(&rec.attributes, "subject_kind"), "seat");
        assert_eq!(str_attr(&rec.attributes, "subject_id"), "1001");
        assert_eq!(str_attr(&rec.attributes, "user_login"), "octocat");
        assert_eq!(
            str_attr(&rec.attributes, "seat_assigned_at"),
            "2026-01-01T00:00:00+00:00"
        );
        assert_eq!(
            str_attr(&rec.attributes, "last_activity_at"),
            "2026-08-01T09:30:00+00:00"
        );
        assert_eq!(
            str_attr(&rec.attributes, "last_activity_editor"),
            "vscode/1.90.0/copilot/1.200.0"
        );
        assert_eq!(str_attr(&rec.attributes, "seat_state"), "active");
    }

    /// A seat that was never used must omit `last_activity_at` /
    /// `last_activity_editor` entirely (unknown, never a fabricated default)
    /// -- RFC-0001's motivating question ("who has a seat and has never used
    /// it") depends on the absence being observable.
    #[test]
    fn never_used_seat_omits_the_activity_attributes() {
        let row = SeatSnapshot {
            provider_user_id: "2002".to_owned(),
            user_login: "neveruser".to_owned(),
            snapshot_day: "2026-08-07".to_owned(),
            seat_assigned_at: Some("2026-01-01T00:00:00Z".parse().unwrap()),
            last_activity_at: None,
            last_activity_editor: None,
            seat_state: "active".to_owned(),
        };
        let rec = encode_seat("t1", "g1", &row);
        assert!(
            !rec.attributes.iter().any(|(k, _)| k == "last_activity_at"),
            "last_activity_at must be omitted, not fabricated"
        );
        assert!(
            !rec.attributes
                .iter()
                .any(|(k, _)| k == "last_activity_editor"),
            "last_activity_editor must be omitted, not fabricated"
        );
        assert_eq!(str_attr(&rec.attributes, "seat_state"), "active");
    }

    /// One record per (report, subject): encoding a single row yields exactly
    /// one `LogRecordData` with the subject's natural key as `subject_id`.
    #[test]
    fn one_record_per_subject() {
        let rows = [
            OrgDaily {
                organization_id: "g1".to_owned(),
                report_day: "2026-08-01".to_owned(),
                active_users: 1,
                engaged_users: 1,
                total_interactions: 1,
                total_completions: 1,
                ai_credits: 0,
                net_cost_micro_usd: MicroUsd(0),
            },
            OrgDaily {
                organization_id: "g2".to_owned(),
                report_day: "2026-08-01".to_owned(),
                active_users: 2,
                engaged_users: 2,
                total_interactions: 2,
                total_completions: 2,
                ai_credits: 0,
                net_cost_micro_usd: MicroUsd(0),
            },
        ];
        let records: Vec<LogRecordData> = rows
            .iter()
            .map(|r| encode_org_daily("t1", "g1", r))
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(str_attr(&records[0].attributes, "subject_id"), "g1");
        assert_eq!(str_attr(&records[1].attributes, "subject_id"), "g2");
    }
}
