//! `drain::quarantine::handle`'s bounded lookahead: a run of TWO OR MORE
//! consecutive permanently-refused records must not wedge the drain forever
//! -- only `serve_otel_quarantine.rs`'s single-record case was covered
//! before this. Split into its own file (not added to that one) purely for
//! the LoC gate.
//!
//! Reproduces the shape of the real incident this fix answers (2026-09-09,
//! `docs/runbooks/otel-daemon-wedged.md`): two consecutive corrupted OTLP
//! records wedged a real daemon's drain at 447+ refusals while 384 good
//! records sat undelivered behind them. Here, two retained records both
//! contain a "poison" marker the mock collector permanently rejects, and a
//! third does not -- proving the fix walks past BOTH poisoned records to
//! find it, not just one.
//!
//! Every admission is durably retained before this binary ever answers `200`
//! (see `otel_daemon::mod`'s module doc) -- forwarding belongs exclusively to
//! the background `pump`, which wakes immediately on a fresh retain and
//! otherwise retries every `PUMP_INTERVAL` (5s). So this polls for the
//! outcome rather than asserting synchronously after a POST, the same
//! discipline `support::interrupt`'s own module doc argues for.

mod support;

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use support::{
    copilot as fixture,
    harness::Harness,
    mock_collector::{Behavior, MockCollector},
    otel_payload::logs_payload,
    serve_otel::Daemon,
};

/// `Quarantine::refused`'s "separate wakes" gate (`MIN_SEPARATION_SECONDS`,
/// `otel_daemon::spool::commit`) is 60 real seconds, read from the actual
/// system clock in the daemon subprocess this test drives -- there is no
/// clock injection at this level (unlike the pure `DurableSpool` unit tests
/// in `otel_daemon::spool::tests::quarantine`, which pin `now` directly).
/// The pump retries every 5s (`PUMP_INTERVAL`) once a record is stuck, so the
/// second real refusal that counts lands somewhere between 60s and ~65s
/// after the first attempt; this budget covers that plus scheduling slop.
const DEADLINE: Duration = Duration::from_secs(100);
const POLL: Duration = Duration::from_millis(500);

async fn until(label: &str, mut ready: impl FnMut() -> Result<bool>) -> Result<()> {
    let deadline = Instant::now() + DEADLINE;
    while Instant::now() < deadline {
        if ready()? {
            return Ok(());
        }
        tokio::time::sleep(POLL).await;
    }
    bail!("timed out after {DEADLINE:?} waiting for {label}")
}

#[tokio::test]
async fn two_consecutive_refused_records_no_longer_wedge_the_drain_forever() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    // Rejects anything containing "poison" from the start -- retention never
    // consults the collector (every admission is durably queued before any
    // forward attempt), so this only ever affects the background pump's own
    // retries, never the POSTs below.
    let collector = MockCollector::start(Behavior::RejectContaining {
        needle: "poison",
        status: 400,
    })
    .await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;
    // Three records land in the spool consecutively. `poison-1` and
    // `poison-2` are permanently unrecoverable; `proof` is not.
    for body in ["poison-1", "poison-2", "proof"] {
        let status = daemon.post("/", &logs_payload(body)).await?;
        assert_eq!(
            status.as_u16(),
            200,
            "every admission is durably retained before any forward attempt: {body}"
        );
    }

    until(
        "both poison-1 and poison-2 to be discarded together, proof delivered",
        || Ok(harness.otel_daemon_discarded_total()? == 2),
    )
    .await?;

    let accepted = collector.accepted_log_bodies()?;
    assert!(
        accepted.iter().any(|body| body == "proof"),
        "proof must have been delivered -- it is what the walk was looking for"
    );
    assert!(
        !accepted.iter().any(|body| body.starts_with("poison")),
        "neither poisoned record may ever be reported as delivered"
    );

    daemon.stop()?;
    Ok(())
}
