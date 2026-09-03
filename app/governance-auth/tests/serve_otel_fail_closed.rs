//! `serve --otel` (issue #268): fail-closed — the daemon never forwards anything
//! it could not authenticate, and a refused/unreachable collector never loses
//! bytes. Together these are A4: the unavailable branch is the restrictive one.
//!
//! Each test drives the real binary as a subprocess ([`support::serve_otel`])
//! against a mock collector. They share the daemon's one fixed loopback port, so
//! they are serialized by the port lock inside [`support::serve_otel::Daemon`].

mod support;

use anyhow::Result;
use support::{
    copilot as fixture,
    harness::Harness,
    interrupt,
    mock_collector::{Behavior, MockCollector},
    otel_payload::logs_payload,
    serve_otel::Daemon,
};

/// No session on disk means the daemon cannot mint a bearer, so it must
/// **withhold**: the client gets a cheap `202`, the collector sees zero
/// requests, and nothing is forwarded unauthenticated. This is the whole
/// fail-closed contract.
#[tokio::test]
async fn no_session_returns_accepted_and_forwards_nothing() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;
    let status = daemon.post("/", &logs_payload("no-session")).await?;

    assert_eq!(
        status.as_u16(),
        202,
        "an unauthenticated payload must still get a cheap accepted, not an error"
    );
    assert_eq!(
        collector.request_count()?,
        0,
        "nothing may be forwarded without a bearer"
    );
    daemon.stop()?;
    Ok(())
}

/// An expired, unrefreshable session is the same fail-closed case one step
/// further in: the mint fails *after* config resolution, and the payload is
/// withheld. (Seeding an expired session with no refresh token means
/// `current_session` cannot recover by calling out to the IdP, so the
/// unreachable issuer never matters.)
#[tokio::test]
async fn an_expired_unrefreshable_session_withholds_and_forwards_nothing() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::expired_session(harness.issuer())?)?;
    let collector = MockCollector::start(Behavior::Accept).await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;
    let status = daemon.post("/", &logs_payload("expired")).await?;

    assert_eq!(status.as_u16(), 202);
    assert_eq!(collector.request_count()?, 0);
    daemon.stop()?;
    Ok(())
}

/// A collector that refuses the export (a retryable 500) must not fail the
/// client: it costs a `202`, never a `500`, and the bytes are retained, not
/// dropped. Then once the collector recovers, the very next accepted request
/// piggybacks a drain of the retained payload before handling its own — so the
/// refused bytes are not lost, just delayed.
#[tokio::test]
async fn a_refusing_collector_accepts_the_client_and_retains_until_it_recovers() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let collector = MockCollector::start(Behavior::Reject(500)).await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;

    // First POST: the collector is refusing, so the forward fails, the payload
    // is retained, and the client still walks away with 202.
    let first = daemon.post("/", &logs_payload("marker-first")).await?;
    assert_eq!(
        first.as_u16(),
        202,
        "a refused forward must still be a 202, not a 500"
    );

    // Now the collector is healthy again. The second POST drains the retained
    // first payload, then forwards its own.
    collector.set_behavior(Behavior::Accept)?;
    let second = daemon.post("/", &logs_payload("marker-second")).await?;
    assert_eq!(
        second.as_u16(),
        200,
        "a healthy collector must yield a 200 for the new payload"
    );

    // The piggyback drain must have re-forwarded the retained payload, so the
    // collector should now have taken both, in order.
    interrupt::until(
        "both the retained and the new payload to reach the collector",
        || {
            let bodies = collector.accepted_log_bodies()?;
            Ok(bodies.iter().any(|b| b == "marker-first")
                && bodies.iter().any(|b| b == "marker-second"))
        },
    )
    .await?;

    daemon.stop()?;
    Ok(())
}
