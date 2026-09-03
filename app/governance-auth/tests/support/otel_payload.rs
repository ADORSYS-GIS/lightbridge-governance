//! OTLP payload + session fixtures for the `serve otel` tests.
//!
//! Split out of [`super::serve_otel`] so that support module stays under the
//! repo's 200-LoC gate: the `Daemon` subprocess driver lives there, the wire
//! shapes it sends live here.

use anyhow::{Context, Result};
use serde_json::{Value, json};

/// A session whose access token is a real JWT (header.payload.signature) so the
/// daemon can stamp identity attributes onto forwarded telemetry. JWT payload is
/// base64url-encoded `{sub, email, preferred_username}`.
pub fn jwt_session(issuer: &str) -> Result<Value> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    let payload = URL_SAFE_NO_PAD.encode(
        br#"{"sub":"user-uuid-1234","email":"dev@example.com","preferred_username":"dev"}"#,
    );
    let access_token = format!("header.{payload}.signature");
    Ok(json!({
        "issuer": issuer,
        "client_id": "test-client",
        "access_token": access_token,
        "refresh_token": null,
        "expires_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("system clock before unix epoch")?
            .as_secs()
            .saturating_add(3600),
    }))
}

/// One OTLP log-record body the collector can grep for, distinct enough that a
/// test can tell two payloads apart in the mock's received list.
pub fn logs_payload(marker: &str) -> Value {
    json!({
        "resourceLogs": [{
            "resource": {
                "attributes": [{
                    "key": "service.name",
                    "value": { "stringValue": "test-client" },
                }],
            },
            "scopeLogs": [{
                "scope": { "name": "test", "version": "0.0.0" },
                "logRecords": [{
                    "timeUnixNano": "1788191912613000000",
                    "body": { "stringValue": marker },
                }],
            }],
        }],
    })
}

/// One OTLP metrics body, so the collector path `/v1/metrics` (not `/v1/logs`)
/// can be asserted.
pub fn metrics_payload() -> Value {
    json!({
        "resourceMetrics": [{
            "resource": { "attributes": [] },
            "scopeMetrics": [{
                "scope": { "name": "test", "version": "0.0.0" },
                "metrics": [{
                    "name": "test.counter",
                    "type": "COUNTER",
                    "dataPoints": [{ "asInt": 1 }],
                }],
            }],
        }],
    })
}
