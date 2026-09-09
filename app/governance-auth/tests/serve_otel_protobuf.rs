//! OTLP protobuf admission and byte-for-byte forwarding.

mod support;

use anyhow::Result;
use support::{
    copilot as fixture, harness::Harness, interrupt, raw_collector::RawCollector,
    serve_otel::Daemon,
};

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
    let response = daemon
        .post_bytes_response(
            "/v1/metrics",
            "application/x-protobuf",
            metrics_bytes.clone(),
        )
        .await?;
    assert_eq!(
        response.status().as_u16(),
        200,
        "a protobuf body must be accepted into durable custody"
    );
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok()),
        Some("application/x-protobuf")
    );
    assert!(response.bytes().await?.is_empty());

    interrupt::until("the protobuf payload to reach the collector", || {
        Ok(collector.requests()?.len() == 1)
    })
    .await?;

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
