//! `serve --otel`'s durable spool (#269): a killed daemon loses nothing it
//! had already accepted, and an outage costs latency, never data.
//!
//! Each test drives the real binary as a subprocess ([`support::serve_otel`])
//! against a mock collector, sharing the daemon's one fixed loopback port
//! (serialized by [`support::serve_otel::Daemon`]'s port lock). The
//! quarantine/probe-discard and idempotency-key tests live in
//! `serve_otel_quarantine.rs`, split out for the LoC gate.

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

/// AC1: killing the daemon for an hour and restarting it loses zero records.
/// Here: retain one payload while the collector is down, `SIGKILL` the
/// daemon, start a fresh one against the same state directory with the
/// collector now healthy, and confirm it is delivered without ever having
/// been re-submitted by a client.
#[tokio::test]
async fn a_payload_retained_before_a_kill_is_delivered_by_the_daemon_restarted_after_it()
-> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let collector = MockCollector::start(Behavior::Reject(503)).await?;

    let first = Daemon::start(&harness, &collector.base_url, &[]).await?;
    let status = first.post("/", &logs_payload("survives-a-kill")).await?;
    assert_eq!(
        status.as_u16(),
        202,
        "retained, not failed, while the collector is down"
    );
    first.stop()?;

    collector.set_behavior(Behavior::Accept)?;
    let second = Daemon::start(&harness, &collector.base_url, &[]).await?;

    interrupt::until(
        "the record retained before the kill to reach the collector after the restart",
        || {
            let bodies = collector.accepted_log_bodies()?;
            Ok(bodies.iter().any(|b| b == "survives-a-kill"))
        },
    )
    .await?;

    second.stop()?;
    Ok(())
}

/// AC2: an unreachable collector retains bytes and delivers them once it
/// returns, with `discarded_total` staying at 0 -- a retryable outage must
/// never be counted as a loss.
#[tokio::test]
async fn an_unreachable_collector_retains_and_delivers_without_touching_discarded_total()
-> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let collector = MockCollector::start(Behavior::Reject(503)).await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;
    daemon.post("/", &logs_payload("outage-marker")).await?;

    assert_eq!(
        harness.otel_daemon_discarded_total()?,
        0,
        "a retryable outage must not be counted as discarded"
    );

    collector.set_behavior(Behavior::Accept)?;
    interrupt::until(
        "the retained payload to be delivered once the collector recovers",
        || {
            let bodies = collector.accepted_log_bodies()?;
            Ok(bodies.iter().any(|b| b == "outage-marker"))
        },
    )
    .await?;

    assert_eq!(
        harness.otel_daemon_discarded_total()?,
        0,
        "delivery after an outage must still read as zero discarded"
    );
    daemon.stop()?;
    Ok(())
}
