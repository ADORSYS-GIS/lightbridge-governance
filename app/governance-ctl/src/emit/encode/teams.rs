//! `user-teams-1-day` encoder.

use governance_copilot::UserTeam;

use super::{AttributeValue, LogRecordData, common};

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
