//! A wake that is killed part way through must keep what it already
//! delivered.
//!
//! The checkpoint used to be written once, at the end of `drain::once`, and the
//! binary installs no signal handler, so a wake killed mid-drain threw away
//! every acceptance it had obtained and the next wake re-sent all of them --
//! into a usage store, where a duplicate is a wrong number. Measured on the
//! code this file was written against: SIGTERM at 1.2s in, 16 records already
//! taken, `checkpoint after the kill: None`, 16 duplicate deliveries once the
//! drain finished. `copilot::journal` is the fix and carries the reasoning.
//!
//! ## What "already delivered" means precisely
//!
//! A response the collector sent and the client never read is genuinely
//! ambiguous, and no checkpointing resolves it -- that record may legitimately
//! be offered again. So the assertions below are restricted to acceptances that
//! are **settled**: the drain is sequential, so once request `k+2` has arrived,
//! responses `k` and `k+1` have provably been read. Those were durably
//! delivered, and re-sending one is a defect rather than an ambiguity.

mod support;

use anyhow::{Context, Result};
use serde_json::Value;
use support::{
    copilot as fixture,
    harness::Harness,
    interrupt::{self, Wake},
    mock_collector::{Behavior, MockCollector},
};

/// Every eighth record is one the collector refuses, so the drain bisects --
/// the only thing that makes a wake more than one request long, and so the only
/// thing that gives it an interior to be killed in.
fn alternating(count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| {
            let marker = if index % 8 == 7 {
                format!("POISON-{index}")
            } else {
                format!("good-{index}")
            };
            fixture::marked_log_line(&marker)
        })
        .collect()
}

fn checkpoint(harness: &Harness) -> Result<Option<Value>> {
    let path = fixture::checkpoint_path(harness);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(
        serde_json::from_slice(&std::fs::read(&path).context("reading the checkpoint")?)
            .context("parsing the checkpoint")?,
    ))
}

fn offset(state: &Option<Value>) -> u64 {
    state
        .as_ref()
        .and_then(|value| value.get("offset")?.as_u64())
        .unwrap_or_default()
}

/// Markers for a spool with nothing the collector objects to, so the only thing
/// that can cost a record is the kill itself.
fn keep_markers() -> impl Iterator<Item = String> {
    (0..12).map(|index| format!("keep-{index:02}"))
}

fn redelivered(settled: &[String], all: &[String]) -> Vec<String> {
    settled
        .iter()
        .filter(|body| all.iter().filter(|seen| seen == body).count() > 1)
        .cloned()
        .collect()
}

/// THE regression test. SIGKILL mid-drain; nothing the collector had already
/// answered for may be offered again.
#[tokio::test]
async fn a_wake_killed_mid_drain_never_re_delivers_what_it_already_got_through() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::RejectContainingSlowly {
        needle: "POISON",
        status: 400,
        millis: 80,
    })
    .await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    fixture::write_spool(&spool, &alternating(40))?;

    let wake = Wake::start(&harness, &collector.base_url, &spool)?;

    // Enough acceptances to be worth losing, early enough to be mid-bisect.
    let taken = &collector;
    interrupt::until("the collector to take three records", || {
        Ok(taken.accepted_log_bodies()?.len() >= 3)
    })
    .await?;
    // Bodies BEFORE the request count, so `mark` can only over-estimate how far
    // the drain got -- erring towards waiting, never towards calling an
    // unsettled acceptance settled.
    let settled = collector.accepted_log_bodies()?;
    let mark = collector.request_count()?;
    interrupt::until(
        "two further requests, which settle the ones before them",
        || Ok(taken.request_count()? > mark + 1),
    )
    .await?;
    wake.kill()?;

    let after_kill = checkpoint(&harness)?;
    assert!(
        offset(&after_kill) > 0,
        "the collector had already taken {} record(s) and the wake was killed with nothing \
         recorded at all ({after_kill:?}) -- the next wake will rebuild and re-send every one of \
         them. Durability that only happens at end-of-wake is not durability.",
        settled.len()
    );

    // Whatever the drain does from here, none of those may go again. The
    // collector stops refusing so finishing costs a couple of wakes rather than
    // one per quarantined record: the bisect has its own tests.
    collector.set_behavior(Behavior::Accept)?;
    let size = std::fs::metadata(&spool).context("sizing the spool")?.len();
    let mut wakes = 0;
    while offset(&checkpoint(&harness)?) < size && wakes < 10 {
        fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
        wakes += 1;
    }

    let all = collector.accepted_log_bodies()?;
    let repeated = redelivered(&settled, &all);
    assert!(
        repeated.is_empty(),
        "{} record(s) the collector had already accepted and answered for were delivered again \
         after the kill: {:?}",
        repeated.len(),
        repeated.iter().take(5).collect::<Vec<_>>()
    );
    assert_eq!(
        offset(&checkpoint(&harness)?),
        size,
        "and the spool must still drain to the end in {wakes} wakes"
    );
    Ok(())
}

/// The other half: a kill must not *lose* anything either.
#[tokio::test]
async fn a_wake_killed_mid_drain_loses_nothing_it_had_not_delivered() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::RejectContainingSlowly {
        needle: "NOTHING-MATCHES-THIS",
        status: 400,
        millis: 120,
    })
    .await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let kept: Vec<Value> = keep_markers()
        .map(|m| fixture::marked_log_line(&m))
        .collect();
    fixture::write_spool(&spool, &kept)?;

    let wake = Wake::start(&harness, &collector.base_url, &spool)?;
    let seen = &collector;
    interrupt::until("the collector to receive the batch", || {
        Ok(seen.request_count()? >= 1)
    })
    .await?;
    wake.kill()?;

    // The collector answers instantly from here, so one wake finishes the job.
    collector.set_behavior(Behavior::Accept)?;
    let output = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    let delivered = collector.accepted_log_bodies()?;
    let lost: Vec<String> = keep_markers()
        .filter(|marker| !delivered.contains(marker))
        .collect();
    assert!(
        lost.is_empty(),
        "a killed wake may cost a duplicate; it may never cost a record: {lost:?}. stderr: \
         {stderr}"
    );
    assert_eq!(
        checkpoint(&harness)?
            .as_ref()
            .and_then(|state| state.get("discarded_total")?.as_u64())
            .unwrap_or_default(),
        0,
        "nothing here was unreadable or refused"
    );
    Ok(())
}
