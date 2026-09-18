//! `organization-1-day`, `users-1-day` and `repos-1-day` encoders.

use anyhow::Result;
use governance_copilot::{OrgDaily, RepoDaily, UserDaily};
use governance_core::MicroUsd;

use super::{AttributeValue, LogRecordData, checked_i64, common};

/// Encode one `organization-1-day` row.
///
/// GitHub's org report carries no cost/credits (those are user-level only), so
/// the org-level spend is passed in explicitly -- aggregated from the day's
/// `users-1-day` rows by the caller (see `crate::emit::Sink::emit_rows`).
/// Without this the org record would silently claim zero spend.
pub fn encode_org_daily(
    tenant_id: &str,
    org: &str,
    row: &OrgDaily,
    org_ai_credits: u64,
    org_net_cost_micro_usd: MicroUsd,
) -> Result<LogRecordData> {
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
        AttributeValue::Int(checked_i64(row.active_users)?),
    ));
    attrs.push((
        "engaged_users".to_owned(),
        AttributeValue::Int(checked_i64(row.engaged_users)?),
    ));
    attrs.push((
        "total_interactions".to_owned(),
        AttributeValue::Int(checked_i64(row.total_interactions)?),
    ));
    attrs.push((
        "total_completions".to_owned(),
        AttributeValue::Int(checked_i64(row.total_completions)?),
    ));
    attrs.push((
        "ai_credits".to_owned(),
        AttributeValue::Int(checked_i64(org_ai_credits)?),
    ));
    attrs.push((
        "net_cost_micro_usd".to_owned(),
        AttributeValue::Int(org_net_cost_micro_usd.0),
    ));
    Ok(LogRecordData {
        body: format!(
            "org {} {}: {} active, {} engaged, {} interactions",
            row.organization_id,
            row.report_day,
            row.active_users,
            row.engaged_users,
            row.total_interactions
        ),
        attributes: attrs,
    })
}

/// Encode one `users-1-day` row.
pub fn encode_user_daily(tenant_id: &str, org: &str, row: &UserDaily) -> Result<LogRecordData> {
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
        AttributeValue::Int(checked_i64(row.total_interactions)?),
    ));
    attrs.push((
        "total_completions".to_owned(),
        AttributeValue::Int(checked_i64(row.total_completions)?),
    ));
    attrs.push((
        "ai_credits".to_owned(),
        AttributeValue::Int(checked_i64(row.ai_credits)?),
    ));
    attrs.push((
        "net_cost_micro_usd".to_owned(),
        AttributeValue::Int(row.net_cost_micro_usd.0),
    ));
    Ok(LogRecordData {
        body: format!(
            "user {} {}: {} interactions, {} completions",
            row.provider_user_id, row.report_day, row.total_interactions, row.total_completions
        ),
        attributes: attrs,
    })
}

/// Encode one `repos-1-day` row.
pub fn encode_repo_daily(tenant_id: &str, org: &str, row: &RepoDaily) -> Result<LogRecordData> {
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
        AttributeValue::Int(checked_i64(row.coding_agent_activity)?),
    ));
    attrs.push((
        "code_review_activity".to_owned(),
        AttributeValue::Int(checked_i64(row.code_review_activity)?),
    ));
    attrs.push((
        "pull_request_activity".to_owned(),
        AttributeValue::Int(checked_i64(row.pull_request_activity)?),
    ));
    Ok(LogRecordData {
        body: format!(
            "repo {} {}: {} coding, {} review, {} pr",
            row.repository_id,
            row.report_day,
            row.coding_agent_activity,
            row.code_review_activity,
            row.pull_request_activity
        ),
        attributes: attrs,
    })
}
