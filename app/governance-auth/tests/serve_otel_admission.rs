//! `serve --otel` (issue #268), #290 review round 2: admission runs before any
//! credentialed work, including draining what is already retained.
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

/// #290 review round 2: an untrusted-Host request must cost nothing
/// credentialed, even when the spool is non-empty. `drain_retained` used to
/// run before the admission check, so a caller that could never be admitted
/// still forced a mint-and-forward of whatever was already retained --
/// defeating `receive`'s own documented "an untrusted request costs
/// nothing" property. Proven here by filling the spool via an unreachable
/// collector, then pointing it at a healthy one and sending an
/// untrusted-Host request: if the drain ran anyway, the healthy collector
/// would see the retained payload.
#[tokio::test]
async fn an_untrusted_host_request_does_not_drain_the_spool() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    // One daemon, one collector, for this whole test: the spool is
    // in-memory-only (#268), so a restart would lose the retained payload
    // itself and prove nothing about whether draining it needs admission.
    let collector = MockCollector::start(Behavior::Reject(500)).await?;
    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;

    let retained = daemon
        .post("/", &logs_payload("should-stay-retained"))
        .await?;
    assert_eq!(
        retained.as_u16(),
        202,
        "fixture: must have retained something"
    );

    // Now the collector WOULD accept the retained payload -- isolating "did
    // the untrusted request's drain attempt run" from whether a healthy
    // collector was ever reachable. `request_count` already includes the
    // rejected attempt above (the mock records every request, accepted or
    // not), so the baseline is taken fresh here rather than assumed to be 0.
    collector.set_behavior(Behavior::Accept)?;
    let before = collector.request_count()?;

    let untrusted = daemon
        .post_with_host(
            "/",
            "attacker.rebound.example:17457",
            &logs_payload("untrusted"),
        )
        .await?;
    assert_eq!(untrusted.as_u16(), 403, "an untrusted Host must be refused");
    assert_eq!(
        collector.request_count()?,
        before,
        "the retained payload must not have been drained by a request that was never admitted"
    );

    // A genuinely admitted request afterward still drains it -- proving the
    // spool held the record rather than having quietly lost it.
    let admitted = daemon.post("/", &logs_payload("admitted")).await?;
    assert_eq!(admitted.as_u16(), 200);
    interrupt::until(
        "the retained payload to reach the collector once admitted",
        || {
            Ok(collector
                .accepted_log_bodies()?
                .iter()
                .any(|b| b == "should-stay-retained"))
        },
    )
    .await?;

    daemon.stop()?;
    Ok(())
}
