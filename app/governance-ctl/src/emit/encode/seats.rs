//! `billing-seats` encoder.

use governance_copilot::SeatSnapshot;

use super::{AttributeValue, LogRecordData, common};

/// Encode one `billing-seats` row (a seat snapshot).
///
/// The usage-side `usage_seat_snapshots` table has no `seat` subject kind: its
/// vocabulary is `org` / `user` / `repo` / `user_team`, and it documents
/// `subject_id` as "the org/team/entity this seat belongs to" with a distinct
/// NOT NULL `provider_user_id` PK column for the seat holder. So a seat
/// snapshot is encoded as an **org** subject (`subject_kind=org`,
/// `subject_id=org`), with the seat holder carried in a dedicated
/// `provider_user_id` attribute the normalizer maps onto that PK. See the
/// RFC-0001 contract and its known-issues entry for the coordinated change.
pub fn encode_seat(tenant_id: &str, org: &str, row: &SeatSnapshot) -> LogRecordData {
    let mut attrs = common(
        tenant_id,
        org,
        "billing-seats",
        &row.snapshot_day,
        "org",
        org,
    );
    attrs.push((
        "provider_user_id".to_owned(),
        AttributeValue::Str(row.provider_user_id.clone()),
    ));
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
