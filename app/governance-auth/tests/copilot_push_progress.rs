//! Progress must be **monotonic**: whatever the collector took on one wake is
//! never offered again on the next, however the wake ends.
//!
//! The regression this file guards is worse than the stall it replaced. The
//! bisect that isolates a permanently-refused record posts accepted
//! sub-batches on the way down, and those acceptances used to be recorded
//! nowhere: if the split then ran out of its request budget, or the collector
//! went away mid-split, the whole signal returned `Err`, no offset moved, and
//! the *next* wake rebuilt and re-sent every record the collector had already
//! taken. Measured on a 512-record spool with 12 refused: 438 good records
//! delivered again on every single wake, for ever. This is usage/billing data
//! -- a duplicate is not a wasted request, it is a wrong number.
//!
//! So the drain resolves a range strictly left to right and advances that
//! signal's offset over the **prefix it resolved**, even when it stops short.
//! The assertions below are on what the collector *accepted*, across two
//! wakes, not on a request count: a request that was refused delivered
//! nothing, and counting it would hide exactly the duplication being tested.

mod support;

use anyhow::{Context, Result};
use serde_json::Value;
use support::{
    copilot as fixture,
    harness::Harness,
    mock_collector::{Behavior, MockCollector},
};

/// Alternating good/refused records. Every range of two or more contains a
/// refused one, so the bisect walks the whole tree -- which is what used to
/// exhaust the request budget and throw the accepted half away.
fn alternating(count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| {
            let marker = if index % 2 == 0 {
                format!("good-{index}")
            } else {
                format!("POISON-{index}")
            };
            fixture::marked_log_line(&marker)
        })
        .collect()
}

fn duplicates(bodies: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut repeated = Vec::new();
    for body in bodies {
        if !seen.insert(body.clone()) {
            repeated.push(body.clone());
        }
    }
    repeated
}

fn offset(harness: &Harness) -> Result<u64> {
    let path = fixture::checkpoint_path(harness);
    if !path.exists() {
        return Ok(0);
    }
    let state: Value =
        serde_json::from_slice(&std::fs::read(&path).context("reading the checkpoint")?)
            .context("parsing the checkpoint")?;
    Ok(state.get("offset").and_then(Value::as_u64).unwrap_or(0))
}

/// THE regression test. Two wakes against a collector that refuses half the
/// records; nothing the collector accepted on wake 1 may be offered again on
/// wake 2, and the offset must have moved.
#[tokio::test]
async fn a_wake_never_re_sends_what_an_earlier_wake_delivered() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::RejectContaining {
        needle: "POISON",
        status: 400,
    })
    .await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    fixture::write_spool(&spool, &alternating(100))?;

    let first = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let delivered_by_first = collector.accepted_log_bodies()?;
    let offset_after_first = offset(&harness)?;
    assert!(
        !delivered_by_first.is_empty(),
        "the fixture is only meaningful if the collector took something on wake 1: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        offset_after_first > 0,
        "wake 1 delivered {} record(s) and then recorded no progress at all -- the next wake will \
         rebuild and re-send every one of them. stderr: {}",
        delivered_by_first.len(),
        String::from_utf8_lossy(&first.stderr)
    );

    let second = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let all = collector.accepted_log_bodies()?;
    let repeated = duplicates(&all);
    assert!(
        repeated.is_empty(),
        "{} record(s) were delivered twice across two wakes; this lands in a usage store, so a \
         duplicate is a wrong number rather than a wasted request. First few: {:?}. stderr: {}",
        repeated.len(),
        repeated.iter().take(5).collect::<Vec<_>>(),
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(
        offset(&harness)? >= offset_after_first,
        "an offset must never go backwards"
    );
    Ok(())
}

/// The other half of "bounded, but not a cliff": each wake must move forward.
/// A budget that stops a wake is fine; a budget that makes it start over is
/// the defect above wearing a different hat.
#[tokio::test]
async fn every_wake_moves_the_offset_forward_until_the_spool_is_drained() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::RejectContaining {
        needle: "POISON",
        status: 400,
    })
    .await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    // An ODD count, so the last record is one the collector accepts. A spool
    // whose final record is refused is deliberately *not* drained to the end:
    // with nothing after it, there is nothing to prove the collector works
    // with, so it waits for the next append rather than being discarded on no
    // evidence. That is the documented behaviour, not a stall this test should
    // paper over -- it just makes "drains completely" the wrong fixture.
    fixture::write_spool(&spool, &alternating(13))?;
    let size = std::fs::metadata(&spool).context("sizing the spool")?.len();

    let mut previous = 0;
    let mut wakes = 0;
    // Generous: the invariant under test is "strictly forward every wake",
    // and the loop exits the moment the spool is drained.
    while offset(&harness)? < size && wakes < 40 {
        let output = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
        wakes += 1;
        let now = offset(&harness)?;
        assert!(
            now > previous,
            "wake {wakes} made no progress: offset stayed at {now} of {size}. stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        previous = now;
    }
    assert_eq!(
        offset(&harness)?,
        size,
        "the spool never drained in {wakes} wakes"
    );
    assert!(
        duplicates(&collector.accepted_log_bodies()?).is_empty(),
        "no record may be delivered twice across the whole drain"
    );
    Ok(())
}
