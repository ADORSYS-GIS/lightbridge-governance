//! `serve --otel` (issue #268): the forwarding surface — any-path admission (A2),
//! signal routing, identity stamping (A6), and protobuf passthrough (the F1 fix).
//!
//! Drives the real binary as a subprocess against a mock (or raw-bytes)
//! collector. Shares the daemon's one fixed loopback port, serialized by the
//! port lock inside [`support::serve_otel::Daemon`].

mod support;

use anyhow::{Context, Result};
use serde_json::json;
use support::{
    copilot as fixture,
    harness::Harness,
    mock_collector::{Behavior, MockCollector},
    otel_payload::{jwt_session, logs_payload, metrics_payload},
    raw_collector::RawCollector,
    serve_otel::Daemon,
};

/// The daemon accepts **any** path on its loopback endpoint (A2) — Codex posts
/// to its configured endpoint verbatim — and routes the body by signal. A POST
/// to `/garbage` with a `resourceLogs` body must still be classified as logs
/// and land on `/v1/logs`, never rejected for the unknown path.
#[tokio::test]
async fn any_path_is_forwarded_by_body_signal_not_rejected() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let collector = MockCollector::start(Behavior::Accept).await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;
    let status = daemon
        .post("/utter-garbage", &logs_payload("any-path"))
        .await?;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(
        collector.paths()?,
        vec!["/v1/logs".to_owned()],
        "the path must be carried, not branch-decisioned; the body routes the signal"
    );
    daemon.stop()?;
    Ok(())
}

/// The full happy path: a metrics body routes to `/v1/metrics`, a logs body to
/// `/v1/logs`, and both are stamped with the caller's identity (`user.id`) from
/// the minted token's JWT claims — the same attribution `copilot push` uses.
#[tokio::test]
async fn metrics_and_logs_are_routed_and_stamped_with_identity() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&jwt_session(harness.issuer())?)?;
    let collector = MockCollector::start(Behavior::Accept).await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;

    assert_eq!(daemon.post("/", &metrics_payload()).await?.as_u16(), 200);
    assert_eq!(
        daemon.post("/", &logs_payload("identity")).await?.as_u16(),
        200
    );

    let payloads = collector.payloads()?;
    assert_eq!(payloads.len(), 2, "one metrics, one logs forward");
    let paths: Vec<&str> = payloads.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(paths, vec!["/v1/metrics", "/v1/logs"]);

    for (_path, payload) in payloads {
        let stamped = payload
            .pointer("/resourceMetrics/0/resource/attributes")
            .or_else(|| payload.pointer("/resourceLogs/0/resource/attributes"))
            .and_then(|a| a.as_array())
            .context("the forwarded payload must carry stamped identity attributes")?;
        let user_id = stamped
            .iter()
            .find(|a| a.pointer("/key").and_then(|k| k.as_str()) == Some("user.id"))
            .and_then(|a| a.pointer("/value/stringValue").and_then(|v| v.as_str()))
            .context("the forwarded payload must carry a user.id attribute")?;
        assert_eq!(
            user_id, "user-uuid-1234",
            "attribution must match the minted token's `sub` claim"
        );
    }

    daemon.stop()?;
    Ok(())
}

/// #290 review, P1-1: a poster that pre-sets `user.id`/`account_id`/
/// `api_key_id` used to have those values forwarded verbatim under this
/// developer's bearer -- the deployed ingest handler reads them straight from
/// the payload, no credential-derived override. Real end-to-end reproduction
/// of the review's own probe, kept as a permanent regression test.
#[tokio::test]
async fn a_forged_identity_attribute_is_replaced_before_forwarding() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&jwt_session(harness.issuer())?)?;
    let collector = MockCollector::start(Behavior::Accept).await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;

    let forged = json!({
        "resourceLogs": [{
            "resource": {
                "attributes": [
                    { "key": "user.id", "value": { "stringValue": "somebody-elses-uuid" } },
                    { "key": "account_id", "value": { "stringValue": "victim-account" } },
                    { "key": "api_key_id", "value": { "stringValue": "victim-key" } },
                ],
            },
            "scopeLogs": [{
                "scope": { "name": "test", "version": "0.0.0" },
                "logRecords": [{
                    "timeUnixNano": "1788191912613000000",
                    "body": { "stringValue": "forgery-attempt" },
                }],
            }],
        }],
    });
    assert_eq!(daemon.post("/", &forged).await?.as_u16(), 200);

    let payloads = collector.payloads()?;
    assert_eq!(payloads.len(), 1);
    let attributes = payloads[0]
        .1
        .pointer("/resourceLogs/0/resource/attributes")
        .and_then(|a| a.as_array())
        .context("forwarded payload must carry attributes")?;

    let user_id = attributes
        .iter()
        .find(|a| a.pointer("/key").and_then(|k| k.as_str()) == Some("user.id"))
        .and_then(|a| a.pointer("/value/stringValue").and_then(|v| v.as_str()));
    assert_eq!(
        user_id,
        Some("user-uuid-1234"),
        "the forged user.id must be replaced by the bearer's real identity, not forwarded"
    );
    for forged_key in ["account_id", "api_key_id"] {
        assert!(
            !attributes
                .iter()
                .any(|a| a.pointer("/key").and_then(|k| k.as_str()) == Some(forged_key)),
            "{forged_key} is never written by this daemon and must be stripped, not forwarded: \
             {attributes:?}"
        );
    }

    daemon.stop()?;
    Ok(())
}

/// A non-JSON (OTLP protobuf) payload must be **forwarded**, not withheld — a
/// real client's default wire format on this daemon. This is the regression
/// tripwire for the JSON-only bug (F1 in the test plan): it accepts raw bytes
/// and asserts the collector got exactly them, routed to the path the URL named,
/// with the protobuf content-type preserved.
#[tokio::test]
async fn a_non_json_body_is_forwarded_verbatim_not_withheld() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let collector = RawCollector::start().await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;

    // A protobuf metrics body at the metrics path.
    let metrics_bytes = b"\x0a\x03log\x12\x04test\x00\x01\x02\x03".to_vec();
    let status = daemon
        .post_bytes(
            "/v1/metrics",
            "application/x-protobuf",
            metrics_bytes.clone(),
        )
        .await?;
    assert_eq!(
        status.as_u16(),
        200,
        "a protobuf body with a valid session must be forwarded (200), not withheld (202)"
    );

    let requests = collector.requests()?;
    assert_eq!(requests.len(), 1, "one protobuf forward expected");
    let (path, content_type, body) = &requests[0];
    assert_eq!(
        path, "/v1/metrics",
        "routed by the URL path for a non-JSON body"
    );
    assert_eq!(
        content_type, "application/x-protobuf",
        "wire format preserved"
    );
    assert_eq!(
        body, &metrics_bytes,
        "forwarded verbatim, not re-serialized"
    );

    daemon.stop()?;
    Ok(())
}
