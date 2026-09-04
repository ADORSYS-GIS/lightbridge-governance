//! `serve --otel`'s durable spool (#269): quarantine/probe-discard (P1-3) and
//! the retried-record idempotency key (P1-4), from the #291 review. Split
//! from `serve_otel_durability.rs` purely for the LoC gate.
//!
//! Each test drives the real binary as a subprocess ([`support::serve_otel`])
//! against a mock collector, sharing the daemon's one fixed loopback port
//! (serialized by [`support::serve_otel::Daemon`]'s port lock).

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

/// #269/#291 review, P1-4: a payload retried out of the durable spool must
/// carry a stable, content-derived key downstream ingest can dedupe an
/// at-least-once redelivery on -- see `normalize`'s module doc, "the
/// idempotency key". Forces a retry (the collector refuses once, then
/// accepts) and inspects what the collector actually received.
#[tokio::test]
async fn a_retried_payload_carries_a_stable_idempotency_key() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let collector = MockCollector::start(Behavior::Reject(503)).await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;
    daemon
        .post("/", &logs_payload("idempotency-marker"))
        .await?;

    collector.set_behavior(Behavior::Accept)?;
    interrupt::until("the retained payload to be delivered", || {
        let bodies = collector.accepted_log_bodies()?;
        Ok(bodies.iter().any(|b| b == "idempotency-marker"))
    })
    .await?;

    let carries_a_retry_key = collector.payloads()?.into_iter().any(|(_, payload)| {
        payload["resourceLogs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|resource| resource.pointer("/resource/attributes"))
            .filter_map(|attributes| attributes.as_array())
            .flatten()
            .any(|attribute| {
                attribute.get("key").and_then(serde_json::Value::as_str)
                    == Some("governance.retry_key")
                    && attribute
                        .pointer("/value/stringValue")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|key| !key.is_empty())
            })
    });
    assert!(
        carries_a_retry_key,
        "a retried record must carry a non-empty governance.retry_key attribute"
    );

    daemon.stop()?;
    Ok(())
}

/// #269/#291 review, P1-3: a record refused on its own across enough
/// separate attempts must still NOT be discarded while nothing has proven
/// the collector accepts anything else -- held, not lost, until it either
/// gets that proof or the collector genuinely recovers. Without the fix,
/// `discarded_total` would tick up to 1 the moment the second refusal
/// landed, and `held-through-refusals` would never reach the collector even
/// once it came back healthy.
#[tokio::test]
async fn a_record_refused_twice_with_nothing_to_probe_stays_held_until_it_can_be_proven()
-> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let collector = MockCollector::start(Behavior::Reject(503)).await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;
    // Retained under a retryable refusal (503, not permanent) -- so it lands
    // in the spool at all, which a permanent refusal on the live path never
    // would (see `serve_otel_fail_closed.rs`'s permanent-refusal test).
    let status = daemon
        .post("/", &logs_payload("held-through-refusals"))
        .await?;
    assert_eq!(status.as_u16(), 202, "retained while unreachable");

    // Now every attempt is a PERMANENT refusal -- the shape that makes the
    // retained record eligible for quarantine on its own two attempts.
    collector.set_behavior(Behavior::Reject(400))?;
    // Two more requests: each one's `drain_retained` retries the held record
    // before handling its own body, so this is two separate refusal
    // attempts against it. Neither live request itself gets retained --
    // a permanent refusal on the *live* path is told to the client directly,
    // never spooled (see the module doc on `otel_daemon::mod`).
    daemon.post("/", &logs_payload("trigger-1")).await?;
    daemon.post("/", &logs_payload("trigger-2")).await?;

    assert_eq!(
        harness.otel_daemon_discarded_total()?,
        0,
        "eligible for discard is not the same as discarded -- nothing has proven the collector \
         accepts anything else yet"
    );

    // The collector recovers for real. The held record is not gone -- it is
    // still the next thing the drain offers, and now succeeds outright.
    collector.set_behavior(Behavior::Accept)?;
    daemon.post("/", &logs_payload("trigger-3")).await?;
    interrupt::until(
        "the held record to be delivered once the collector is genuinely healthy",
        || {
            let bodies = collector.accepted_log_bodies()?;
            Ok(bodies.iter().any(|b| b == "held-through-refusals"))
        },
    )
    .await?;
    assert_eq!(
        harness.otel_daemon_discarded_total()?,
        0,
        "a record that was eventually delivered must never have been counted as discarded"
    );

    daemon.stop()?;
    Ok(())
}
