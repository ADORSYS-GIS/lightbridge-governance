//! Retry-key stamping/stripping (#269/#291 review round 2, P2): the
//! `idempotency_key` half of `stamp` shipped with no test at all. Split from
//! [`super`] purely for the LoC gate.

use super::*;

/// This pins the half that matters for correctness -- a drain-stamped key
/// actually lands as `governance.retry_key`.
#[test]
fn a_supplied_idempotency_key_is_stamped_as_the_retry_key_attribute() {
    let body = br#"{"resourceLogs":[{"resource":{"attributes":[]}}]}"#;
    let stamped = stamp(
        serde_json::from_slice(body).ok(),
        body,
        &token(),
        Some("base64key-of-record"),
    )
    .expect("stamp");
    let value: Value = serde_json::from_slice(&stamped).expect("re-parse");
    let attrs = value["resourceLogs"][0]["resource"]["attributes"]
        .as_array()
        .unwrap();
    let retry_keys: Vec<_> = attrs
        .iter()
        .filter(|attribute| attribute["key"] == RETRY_KEY_ATTRIBUTE)
        .filter_map(|attribute| attribute["value"]["stringValue"].as_str())
        .collect();
    assert_eq!(retry_keys, vec!["base64key-of-record"]);
}

/// The other half of the same finding, and the one the module doc presents as
/// the actual guard against griefing: a client-supplied `governance.retry_key`
/// must never survive to be forwarded as this record's dedup key -- an
/// attacker naming a real record's key could otherwise collide it on purpose.
#[test]
fn a_forged_retry_key_attribute_is_replaced_by_this_modules_own_value() {
    let body = format!(
        r#"{{"resourceLogs":[{{"resource":{{"attributes":[{{"key":"{RETRY_KEY_ATTRIBUTE}","value":{{"stringValue":"attacker-chosen-key"}}}}]}}}}]}}"#
    );
    let stamped = stamp(
        serde_json::from_slice(body.as_bytes()).ok(),
        body.as_bytes(),
        &token(),
        Some("the-real-key"),
    )
    .expect("stamp");
    let value: Value = serde_json::from_slice(&stamped).expect("re-parse");
    let attrs = value["resourceLogs"][0]["resource"]["attributes"]
        .as_array()
        .unwrap();
    let retry_keys: Vec<_> = attrs
        .iter()
        .filter(|attribute| attribute["key"] == RETRY_KEY_ATTRIBUTE)
        .filter_map(|attribute| attribute["value"]["stringValue"].as_str())
        .collect();
    assert_eq!(
        retry_keys,
        vec!["the-real-key"],
        "a forged retry key must never survive to be forwarded as the dedup key"
    );
}

/// Even with no real key to replace it (a live pass-through, `None`), a
/// forged one must still be stripped -- the strip is unconditional, exactly
/// like the identity-attribute forgery rule it mirrors.
#[test]
fn a_forged_retry_key_attribute_is_stripped_even_with_nothing_to_replace_it() {
    let body = format!(
        r#"{{"resourceLogs":[{{"resource":{{"attributes":[{{"key":"{RETRY_KEY_ATTRIBUTE}","value":{{"stringValue":"attacker-chosen-key"}}}}]}}}}]}}"#
    );
    let stamped = stamp_body(body.as_bytes(), &token()).expect("stamp");
    let value: Value = serde_json::from_slice(&stamped).expect("re-parse");
    let attrs = value["resourceLogs"][0]["resource"]["attributes"]
        .as_array()
        .unwrap();
    assert!(
        attrs
            .iter()
            .all(|attribute| attribute["key"] != RETRY_KEY_ATTRIBUTE),
        "a forged retry key must be stripped even when nothing replaces it: {attrs:?}"
    );
}
