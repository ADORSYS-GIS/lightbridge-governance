//! Tests for identity-attribute stamping, including the #290 review's P1-1 fix.
//! Split out of `mod.rs` purely for the LoC ceiling.

use super::*;

fn token() -> Redacted<String> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"user-uuid","email":"dev@example.com"}"#);
    Redacted::new(format!("header.{payload}.signature"))
}

#[test]
fn stamps_identity_onto_a_logs_payload() {
    let body = br#"{"resourceLogs":[{"resource":{"attributes":[]}}]}"#;
    let stamped = stamp(body, &token()).expect("stamp");
    let value: Value = serde_json::from_slice(&stamped).expect("re-parse");
    let attrs = value["resourceLogs"][0]["resource"]["attributes"]
        .as_array()
        .unwrap();
    let keys: Vec<&str> = attrs.iter().filter_map(|a| a["key"].as_str()).collect();
    assert!(keys.contains(&"user.id"));
    assert!(keys.contains(&"user.email"));
}

#[test]
fn stamps_identity_onto_a_metrics_payload() {
    let body = br#"{"resourceMetrics":[{"resource":{"attributes":[]}}]}"#;
    let stamped = stamp(body, &token()).expect("stamp");
    let value: Value = serde_json::from_slice(&stamped).expect("re-parse");
    assert!(
        value["resourceMetrics"][0]["resource"]["attributes"]
            .as_array()
            .is_some_and(|a| a.iter().any(|x| x["key"] == "user.id"))
    );
}

/// The reproduction for the review finding this replaced: a poster that
/// pre-sets `user.id` used to have that value forwarded verbatim under
/// this developer's bearer, and the deployed ingest handler reads
/// `user.id` straight from the payload with no credential-derived
/// override. Now the client-supplied value is stripped and this
/// module's own (JWT-derived, trustworthy) value takes its place.
#[test]
fn a_client_supplied_user_id_is_replaced_not_forwarded() {
    let body = br#"{"resourceLogs":[{"resource":{"attributes":[{"key":"user.id","value":{"stringValue":"somebody-elses-uuid"}}]}}]}"#;
    let stamped = stamp(body, &token()).expect("stamp");
    let value: Value = serde_json::from_slice(&stamped).expect("re-parse");
    let attrs = value["resourceLogs"][0]["resource"]["attributes"]
        .as_array()
        .unwrap();
    let ids: Vec<_> = attrs
        .iter()
        .filter(|a| a["key"] == "user.id")
        .filter_map(|a| a["value"]["stringValue"].as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["user-uuid"],
        "the forged value must be gone, replaced by the bearer's own identity"
    );
}

/// The other half of the same finding: `account_id`/`api_key_id`/`azp`
/// are never written by this module, but the deployed ingest handler
/// reads them from the payload too -- so a poster forging them must be
/// stripped even though nothing here replaces them.
#[test]
fn forged_account_and_api_key_attributes_are_stripped_even_though_nothing_replaces_them() {
    let body = br#"{"resourceLogs":[{"resource":{"attributes":[
        {"key":"account_id","value":{"stringValue":"victim-account"}},
        {"key":"api_key_id","value":{"stringValue":"victim-key"}},
        {"key":"azp","value":{"stringValue":"victim-client"}}
    ]}}]}"#;
    let stamped = stamp(body, &token()).expect("stamp");
    let value: Value = serde_json::from_slice(&stamped).expect("re-parse");
    let attrs = value["resourceLogs"][0]["resource"]["attributes"]
        .as_array()
        .unwrap();
    for forged in ["account_id", "api_key_id", "azp"] {
        assert!(
            !attrs.iter().any(|a| a["key"] == forged),
            "{forged} must be stripped: {attrs:?}"
        );
    }
}

/// The strip is unconditional on having a replacement: an opaque token
/// still removes a forged identity attribute, it just adds nothing of
/// its own back.
#[test]
fn an_opaque_token_still_strips_a_forged_attribute() {
    let body = br#"{"resourceLogs":[{"resource":{"attributes":[{"key":"user.id","value":{"stringValue":"forged"}}]}}]}"#;
    let stamped = stamp(body, &Redacted::new("not-a-jwt".to_owned())).expect("stamp");
    let value: Value = serde_json::from_slice(&stamped).expect("re-parse");
    let attrs = value["resourceLogs"][0]["resource"]["attributes"]
        .as_array()
        .unwrap();
    assert!(
        attrs.iter().all(|a| a["key"] != "user.id"),
        "an opaque token adds no identity of its own, but must still strip a forged one: \
         {attrs:?}"
    );
}

#[test]
fn an_opaque_token_with_no_forged_attributes_yields_original_bytes() {
    let body = br#"{"resourceLogs":[{}]}"#.to_vec();
    let stamped = stamp(&body, &Redacted::new("not-a-jwt".to_owned())).expect("stamp");
    assert_eq!(
        stamped, body,
        "nothing to add and nothing to strip => bytes pass through unchanged"
    );
}

#[test]
fn an_unrecognised_body_is_left_unchanged() {
    let body = br#"{"anythingGoes":true}"#.to_vec();
    let stamped = stamp(&body, &token()).expect("stamp");
    assert_eq!(stamped, body, "no resource list => forward unchanged");
}

#[test]
fn a_non_json_body_passes_through_unchanged() {
    // OTLP protobuf (or any non-JSON wire format) must be forwarded verbatim,
    // not withheld: identity stamping is a label, not an admission gate, and
    // the bearer was already minted before this ran.
    let body = b"\x0a\x03log\x12\x04test".to_vec();
    assert_eq!(
        stamp(&body, &token()).expect("stamp"),
        body,
        "protobuf must pass through unchanged"
    );
    // Also when the token is opaque, so the parse is real and not short-circuited.
    assert_eq!(
        stamp(&body, &Redacted::new("opaque".to_owned())).expect("stamp"),
        body
    );
}
