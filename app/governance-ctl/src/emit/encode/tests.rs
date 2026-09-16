//! Round-trip tests for the daily encoders (RFC-0001 contract).

use governance_copilot::{OrgDaily, RepoDaily, UserDaily};
use governance_core::MicroUsd;

use super::{
    super::{encode_org_daily, encode_repo_daily, encode_user_daily},
    test_util::{int, str_attr},
};

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
    assert_eq!(str_attr(&rec.attributes, "source"), "github-copilot");
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

/// `users-1-day` worked example from the RFC-0001 contract: money is integer
/// micro-USD (25000 for 2.5 credits), never a float.
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

/// One record per (report, subject): encoding a single row yields exactly one
/// `LogRecordData` with the subject's natural key as `subject_id`.
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
    let records: Vec<_> = rows
        .iter()
        .map(|r| encode_org_daily("t1", "g1", r))
        .collect();
    assert_eq!(records.len(), 2);
    assert_eq!(str_attr(&records[0].attributes, "subject_id"), "g1");
    assert_eq!(str_attr(&records[1].attributes, "subject_id"), "g2");
}
