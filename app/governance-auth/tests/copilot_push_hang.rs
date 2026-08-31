//! A collector that accepts the connection and then says nothing must not
//! wedge the drain for ever.
//!
//! ⚠️ This is the whole failure, and it needs three layers to fix because it
//! needs all three to happen:
//!
//! 1. The shared `reqwest::Client` had **no timeout of any kind**, so a POST
//!    to a listener that never answers blocks until the process is killed.
//! 2. The drain holds `copilot-push.lock` across that POST, and the lock's
//!    waiter never times out on a holder it has confirmed is *alive* -- so one
//!    stuck process turns into a permanently stuck drain, not one lost wake.
//! 3. The sample systemd unit is `Type=oneshot` with no `TimeoutStartSec=`,
//!    which systemd defaults to **infinity**, so nothing ever kills the wedged
//!    wake either.
//!
//! Measured before the fix: run 1 against a silent listener never returned;
//! run 2 against a perfectly healthy collector was still blocked at 30s and
//! that collector received **zero** requests. After it: run 1 fails on its own
//! and run 2 drains normally.
//!
//! This test costs real wall-clock time -- it has to, because what it asserts
//! is that a timeout fires. It is bounded well above the configured read
//! timeout so a slow CI runner cannot make it flaky.

mod support;

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use support::{
    copilot as fixture,
    harness::Harness,
    mock_collector::{Behavior, MockCollector},
};

/// The bound the run must finish inside. Generous against the client's own
/// read timeout: this test is about "a timeout exists", not about its value.
const MUST_FINISH_WITHIN: Duration = Duration::from_secs(180);

/// Accepts TCP and never writes a byte back -- a collector mid-deadlock, a
/// half-open connection a NAT stopped forwarding, a proxy waiting on an
/// upstream that will not answer.
async fn silent_listener() -> Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding the silent listener")?;
    let addr = listener
        .local_addr()
        .context("reading the silent listener's address")?;
    let handle = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            // Kept alive deliberately: dropping the stream would send a FIN
            // and the client would fail fast for the wrong reason.
            held.push(stream);
        }
    });
    Ok((format!("http://{addr}"), handle))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_collector_that_never_answers_does_not_wedge_the_next_wake() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let (silent, listener) = silent_listener().await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    let started = Instant::now();
    let stuck = fixture::push(&harness, &silent, &spool, &[]).await?;
    let took = started.elapsed();
    assert!(
        took < MUST_FINISH_WITHIN,
        "the run against a silent collector took {took:?}; with no client timeout it never \
         returns at all"
    );
    assert!(
        !stuck.status.success(),
        "a collector that answered nothing did not accept anything"
    );
    assert!(
        !fixture::checkpoint_path(&harness).exists(),
        "nothing was delivered, so nothing may be recorded as delivered"
    );

    // The lock must have come back with the process, and the next wake must be
    // an ordinary one.
    let collector = MockCollector::start(Behavior::Accept).await?;
    let healthy = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(
        healthy.status.success(),
        "the wake after a stuck one must be ordinary: {}",
        String::from_utf8_lossy(&healthy.stderr)
    );
    assert!(
        collector.request_count()? > 0,
        "the healthy collector received nothing -- the drain is still wedged behind the stuck run"
    );

    listener.abort();
    Ok(())
}
