//! `organization-1-day`, `users-1-day` and `repos-1-day` encoders.

use governance_copilot::{OrgDaily, RepoDaily, UserDaily};

use super::{AttributeValue, LogRecordData, common};

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
