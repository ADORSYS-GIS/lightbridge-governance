//! Durable-spool capacity and sender backpressure.

mod support;

use anyhow::Result;
use support::{
    copilot as fixture, harness::Harness, otel_payload::logs_payload, serve_otel::Daemon,
};

/// #290 review, P1-3: a failed `retain` (the spool genuinely full) must not
/// be reported to the client as success -- that would tell an exporter
/// "delivered" while its only copy was dropped, the unavailable branch
/// becoming the permissive one.
#[tokio::test]
async fn a_full_spool_answers_503_not_success() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    // Unreachable (connection refused), not `Reject`: every byte sent here
    // must retain, none discarded as a permanent refusal, so the spool
    // actually fills.
    let collector_base = "http://127.0.0.1:1".to_owned();

    let daemon = Daemon::start(&harness, &collector_base, &[]).await?;

    // 3 MiB raw per POST: comfortably under the per-record ceiling
    // (`otel_daemon::spool::MAX_RETAINABLE_PAYLOAD`, ~6 MiB once base64 and
    // the JSON envelope are accounted for -- #269/#291 review, P2-5), so
    // each one is retained on its own merits, and small enough that several
    // are needed to reach the 16 MiB aggregate `CAPACITY`. A single POST
    // near 16 MiB (the old fixture here) is now correctly refused at the
    // door with 413 before it ever reaches the spool, which is P2-5's whole
    // point -- so filling capacity now takes several POSTs, not one.
    let chunk = "x".repeat(3 * 1024 * 1024);
    let mut retained = 0;
    let response = loop {
        let response = daemon.post_response("/", &logs_payload(&chunk)).await?;
        if response.status().as_u16() != 200 {
            break response;
        }
        retained += 1;
        assert!(retained <= 10, "capacity should have refused by now");
    };
    assert_eq!(
        response.status().as_u16(),
        503,
        "a spool that could not retain the payload must not answer success"
    );
    assert_eq!(
        response
            .headers()
            .get("retry-after")
            .and_then(|header| header.to_str().ok()),
        Some("5"),
        "backpressure must tell the sender when to retry"
    );
    assert!(retained > 0, "some room must exist below capacity");

    daemon.stop()?;
    Ok(())
}
