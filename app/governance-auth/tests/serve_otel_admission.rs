//! `serve --otel` (issue #268), #290 review round 2: admission runs before any
//! durable or credentialed work.
//!
//! Split out of `serve_otel_fail_closed.rs` purely for the LoC ceiling — this
//! is still a fail-closed property (an unadmitted caller must cost nothing
//! credentialed), just one specific to the admission/drain ordering rather
//! than to spool capacity or collector refusal.

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

/// An untrusted-Host request must never enter durable custody. Restarting the
/// daemon afterward with a valid session and healthy collector proves there
/// is no rejected payload waiting to be forwarded.
#[tokio::test]
async fn an_untrusted_host_request_never_enters_the_spool() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let first = Daemon::start(&harness, &collector.base_url, &[]).await?;

    let untrusted = first
        .post_with_host(
            "/",
            "attacker.rebound.example:17457",
            &logs_payload("untrusted"),
        )
        .await?;
    assert_eq!(untrusted.as_u16(), 403, "an untrusted Host must be refused");
    first.stop()?;

    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let second = Daemon::start(&harness, &collector.base_url, &[]).await?;
    assert_eq!(
        collector.request_count()?,
        0,
        "a request rejected by admission must leave nothing durable to forward"
    );

    let admitted = second.post("/", &logs_payload("admitted")).await?;
    assert_eq!(admitted.as_u16(), 200);
    interrupt::until(
        "the genuinely admitted payload to reach the collector",
        || {
            Ok(collector
                .accepted_log_bodies()?
                .iter()
                .any(|b| b == "admitted"))
        },
    )
    .await?;

    second.stop()?;
    Ok(())
}
