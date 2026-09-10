use super::*;

#[test]
fn traces_strip_forged_identity_and_receive_the_retry_key() {
    let body = br#"{"resourceSpans":[{"resource":{"attributes":[{"key":"user.id","value":{"stringValue":"forged"}}]}}]}"#;
    let bytes = stamp(
        serde_json::from_slice(body).ok(),
        body,
        &token(),
        Some("retry"),
    )
    .expect("stamp traces");
    let value: Value = serde_json::from_slice(&bytes).expect("json");
    let attrs = value["resourceSpans"][0]["resource"]["attributes"]
        .as_array()
        .expect("attributes");
    assert!(!attrs.iter().any(|a| a["value"]["stringValue"] == "forged"));
    assert!(
        attrs
            .iter()
            .any(|a| a["key"] == RETRY_KEY_ATTRIBUTE && a["value"]["stringValue"] == "retry")
    );
}
