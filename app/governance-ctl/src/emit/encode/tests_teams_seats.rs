//! Round-trip tests for the teams and seats encoders (RFC-0001 contract).

use governance_copilot::{SeatSnapshot, UserTeam};

use super::{encode_seat, encode_user_team, test_util::str_attr};

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

/// `billing-seats` worked example from the RFC-0001 contract, including the
/// optional timestamp/editor attributes.
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
    assert_eq!(str_attr(&rec.attributes, "subject_kind"), "org");
    assert_eq!(str_attr(&rec.attributes, "subject_id"), "g1");
    assert_eq!(str_attr(&rec.attributes, "provider_user_id"), "1001");
    assert_eq!(str_attr(&rec.attributes, "user_login"), "octocat");
    assert_eq!(
        str_attr(&rec.attributes, "seat_assigned_at"),
        "2026-01-01T00:00:00Z"
    );
    assert_eq!(
        str_attr(&rec.attributes, "last_activity_at"),
        "2026-08-01T09:30:00Z"
    );
    assert_eq!(
        str_attr(&rec.attributes, "last_activity_editor"),
        "vscode/1.90.0/copilot/1.200.0"
    );
    assert_eq!(str_attr(&rec.attributes, "seat_state"), "active");
}

/// A seat that was never used must omit `last_activity_at` /
/// `last_activity_editor` entirely (unknown, never a fabricated default) --
/// RFC-0001's motivating question ("who has a seat and has never used it")
/// depends on the absence being observable.
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
