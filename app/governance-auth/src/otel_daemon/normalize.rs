//! Stamps identity attributes onto forwarded OTLP payloads (A6).
//!
//! Uses the **same** [`crate::otel::identity_attributes`] that `login` /
//! `configure` / `copilot push` use, so attribution cannot regress or drift
//! between the daemon and the drain. The access token is read (its JWT payload
//! claims) purely to *label* outgoing telemetry; the collector re-derives
//! trusted identity from the authenticated credential, never from these
//! attributes (RFC-0002 trust boundary).

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::{otel, redacted::Redacted};

/// Stamps identity attributes into an OTLP JSON payload, returning the
/// re-serialized bytes.
///
/// On no extractable identity (an opaque or non-JWT token), the original bytes
/// are returned unchanged — losing the `user.id` label is acceptable, failing
/// the forward is not.
///
/// On a body that is **not JSON** (e.g. OTLP protobuf), the original bytes are
/// passed through **unchanged rather than an error**, so a real client's default
/// wire format is still forwarded. Identity stamping is a best-effort label, not
/// an admission gate: withholding a valid payload merely because we cannot parse
/// it to add attributes turns an unhandled format into data loss, and it is
/// **not** an authentication refusal — the bearer was already minted before this
/// runs (A4). Attribution is lost for protobuf payloads, but the governed
/// collector re-derives trusted identity from the authenticated credential
/// anyway (RFC-0002 trust boundary), so the missing label is not a security
/// regression.
pub fn stamp(body: &[u8], access_token: &Redacted<String>) -> Result<Vec<u8>> {
    let attributes = otel::identity_attributes(access_token.expose());
    if attributes.is_empty() {
        return Ok(body.to_vec());
    }

    let Ok(mut value) = serde_json::from_slice::<Value>(body) else {
        // Not JSON (e.g. OTLP protobuf): forward the original unchanged, unstamped.
        return Ok(body.to_vec());
    };

    let mut stamped_any = false;
    for key in ["resourceMetrics", "resourceLogs"] {
        if let Some(resources) = value.get_mut(key).and_then(Value::as_array_mut) {
            for resource in resources {
                stamped_any |= stamp_resource(resource, &attributes);
            }
        }
    }

    if !stamped_any {
        // No recognisable resource list in this body; forward it unchanged
        // rather than inventing structure.
        return Ok(body.to_vec());
    }

    serde_json::to_vec(&value).context("serializing stamped OTLP payload")
}

/// Stamps attributes into one `resourceMetrics[]`/`resourceLogs[]` entry.
/// Returns whether anything was stamped.
fn stamp_resource(resource: &mut Value, attributes: &BTreeMap<String, String>) -> bool {
    let Some(resource_obj) = resource.get_mut("resource").and_then(Value::as_object_mut) else {
        return false;
    };
    let Some(attrs) = resource_obj
        .entry("attributes")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
    else {
        return false;
    };

    let mut stamped = false;
    for (key, value) in attributes {
        // Never overwrite an attribute the client already set.
        let exists = attrs.iter().any(|a| {
            a.as_object()
                .and_then(|o| o.get("key"))
                .and_then(Value::as_str)
                == Some(key)
        });
        if exists {
            continue;
        }
        attrs.push(Value::Object(serde_json::Map::from_iter([
            ("key".to_owned(), Value::String(key.clone())),
            (
                "value".to_owned(),
                Value::Object(serde_json::Map::from_iter([(
                    "stringValue".to_owned(),
                    Value::String(value.clone()),
                )])),
            ),
        ])));
        stamped = true;
    }
    stamped
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn does_not_overwrite_an_existing_attribute() {
        let body = br#"{"resourceLogs":[{"resource":{"attributes":[{"key":"user.id","value":{"stringValue":"existing"}}]}}]}"#;
        let stamped = stamp(body, &token()).expect("stamp");
        let value: Value = serde_json::from_slice(&stamped).expect("re-parse");
        let attrs = value["resourceLogs"][0]["resource"]["attributes"]
            .as_array()
            .unwrap();
        // Exactly one user.id, the pre-existing one.
        let ids: Vec<_> = attrs
            .iter()
            .filter(|a| a["key"] == "user.id")
            .filter_map(|a| a["value"]["stringValue"].as_str())
            .collect();
        assert_eq!(ids, vec!["existing"]);
    }

    #[test]
    fn an_opaque_token_yields_original_bytes() {
        let body = br#"{"resourceLogs":[{}]}"#.to_vec();
        let stamped = stamp(&body, &Redacted::new("not-a-jwt".to_owned())).expect("stamp");
        assert_eq!(stamped, body, "no identity => bytes pass through unchanged");
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
}
