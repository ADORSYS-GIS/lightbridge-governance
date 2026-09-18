//! Tests for the OTLP `Sink::emit_rows` path (encode -> OTLP -> stats
//! accounting) using an in-memory exporter, so the emit path is exercised
//! without a network.

use governance_copilot::{OrgDaily, SeatSnapshot, UserDaily, UserTeam};
use governance_core::MicroUsd;
use opentelemetry_sdk::logs::InMemoryLogExporter;

use super::*;

fn user_row(id: &str, credits: u64, cost: i64) -> UserDaily {
    UserDaily {
        provider_user_id: id.to_owned(),
        user_login: format!("user{id}"),
        report_day: "2026-08-01".to_owned(),
        total_interactions: 1,
        total_completions: 1,
        ai_credits: credits,
        net_cost_micro_usd: MicroUsd(cost),
    }
}

fn org_row() -> OrgDaily {
    OrgDaily {
        organization_id: "g1".to_owned(),
        report_day: "2026-08-01".to_owned(),
        active_users: 1,
        engaged_users: 1,
        total_interactions: 1,
        total_completions: 1,
        ai_credits: 0,
        net_cost_micro_usd: MicroUsd(0),
    }
}

/// Build a `Sink` over an in-memory exporter so the emit path (encode ->
/// OTLP -> stats accounting) is exercised without a network.
fn in_memory_sink() -> (Sink, InMemoryLogExporter) {
    let exporter = InMemoryLogExporter::default();
    let provider = SdkLoggerProvider::builder()
        .with_log_processor(BatchLogProcessor::builder(exporter.clone()).build())
        .build();
    (Sink::from_provider(provider), exporter)
}

/// A successful emit counts the records as accepted (AC 7) and actually
/// delivers them to the exporter.
#[tokio::test]
async fn emit_rows_counts_accepted_and_delivers_records() {
    let (sink, exporter) = in_memory_sink();
    let rows = vec![ParsedRows::User(vec![user_row("1001", 2, 25_000)])];

    let n = sink.emit_rows("t1", "g1", &rows, false).await.unwrap();
    assert_eq!(n, 1);

    let (accepted, rejected) = sink.stats();
    assert_eq!(rejected, 0);
    assert_eq!(accepted, vec![("users-1-day".to_owned(), 1)]);

    let emitted = exporter.get_emitted_logs().unwrap();
    assert_eq!(
        emitted.len(),
        1,
        "the record must actually reach the exporter"
    );
}

/// The org-level cost is aggregated from the day's user rows and lands on
/// the org record (M1) -- the org report itself carries no cost.
#[tokio::test]
async fn emit_rows_aggregates_org_cost_from_user_rows() {
    let (sink, exporter) = in_memory_sink();
    let rows = vec![
        ParsedRows::User(vec![
            user_row("1001", 2, 25_000),
            user_row("1002", 5, 50_000),
        ]),
        ParsedRows::Org(vec![org_row()]),
    ];

    let n = sink.emit_rows("t1", "g1", &rows, false).await.unwrap();
    assert_eq!(n, 3);

    let emitted = exporter.get_emitted_logs().unwrap();
    assert_eq!(emitted.len(), 3);
    // Find the org record and assert its aggregated cost.
    let org_attrs: Vec<(String, AnyValue)> = emitted
        .iter()
        .map(|l| {
            l.record
                .attributes_iter()
                .map(|(k, v)| (k.as_str().to_owned(), v.clone()))
                .collect()
        })
        .find(|attrs: &Vec<(String, AnyValue)>| {
            attrs.iter().any(|(k, v)| {
                k == "report"
                    && matches!(v, AnyValue::String(s) if s.as_str() == "organization-1-day")
            })
        })
        .expect("an org record must be emitted");
    let ai = org_attrs
        .iter()
        .find(|(k, _)| k == "ai_credits")
        .map(|(_, v)| v)
        .expect("ai_credits present");
    let cost = org_attrs
        .iter()
        .find(|(k, _)| k == "net_cost_micro_usd")
        .map(|(_, v)| v)
        .expect("net_cost_micro_usd present");
    assert_eq!(ai, &AnyValue::Int(7));
    assert_eq!(cost, &AnyValue::Int(75_000));
}

/// `user-teams-1-day` is not cut over: the authz-side receiver refuses it
/// (RFC-0001 known-issue #1), so the emitter must skip it entirely rather
/// than emit records the receiver would reject (which would break the
/// cutover count assertions).
#[tokio::test]
async fn user_teams_is_skipped_not_emitted() {
    let (sink, exporter) = in_memory_sink();
    let rows = vec![ParsedRows::UserTeam(vec![
        UserTeam {
            user_id: "1001".to_owned(),
            team_id: "9001".to_owned(),
            team_slug: "eng-platform".to_owned(),
            report_day: "2026-08-01".to_owned(),
        },
        UserTeam {
            user_id: "1001".to_owned(),
            team_id: "9002".to_owned(),
            team_slug: "eng-mobile".to_owned(),
            report_day: "2026-08-01".to_owned(),
        },
    ])];

    let n = sink.emit_rows("t1", "g1", &rows, true).await.unwrap();
    assert_eq!(n, 0, "user-teams-1-day must not be emitted");
    let emitted = exporter.get_emitted_logs().unwrap();
    assert!(
        emitted.is_empty(),
        "no user-teams records may reach the exporter"
    );
}

/// A duplicate seat holder (`provider_user_id`) within one snapshot is a
/// genuine bug that would collapse two seats into one row on the seat
/// natural key. It is a warning in shadow mode but a hard error under the
/// cutover freeze.
#[tokio::test]
async fn duplicate_seat_holder_warns_in_shadow_and_fails_in_freeze() {
    let (sink, _exporter) = in_memory_sink();
    let seat = |id: &str| SeatSnapshot {
        provider_user_id: id.to_owned(),
        user_login: format!("user{id}"),
        snapshot_day: "2026-08-07".to_owned(),
        seat_assigned_at: None,
        last_activity_at: None,
        last_activity_editor: None,
        seat_state: "active".to_owned(),
    };
    let rows = vec![ParsedRows::Seat(vec![seat("1001"), seat("1001")])];

    // Shadow mode: warns, still emits.
    let n = sink.emit_rows("t1", "g1", &rows, false).await.unwrap();
    assert_eq!(n, 2);

    // Freeze mode: refuses loudly.
    let err = sink.emit_rows("t1", "g1", &rows, true).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("collision"),
        "freeze must refuse on a duplicate seat holder: {err:#}"
    );
}
