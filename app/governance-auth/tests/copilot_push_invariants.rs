//! The four properties every earlier round established, re-measured at scale.
//!
//! These are slow and deliberately so: they are the ones a regression would hide
//! from the smaller, faster tests. Each prints its measurement, so the numbers in
//! a review are read off a run rather than asserted from memory.
//!
//! - **Conservation.** Every record is delivered or counted in `discarded_total`;
//!   the two together must account for the whole spool.
//! - **Zero duplicates across wakes**, at 2,600 records with 10 refused and at
//!   512 with 12.
//! - **The two-separate-wakes rule** still gates every discard.
//!
//! The fourth -- concurrent wakes costing one wake's requests -- lives with the
//! rest of the locking in `copilot_push_concurrency.rs`.

mod support;

use anyhow::{Context, Result};
use serde_json::Value;
use support::{
    copilot as fixture,
    harness::Harness,
    mock_collector::{Behavior, MockCollector},
};

fn spool_with(count: usize, refused: &[usize]) -> Vec<Value> {
    (0..count)
        .map(|index| {
            let marker = if refused.contains(&index) {
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

fn number(state: &Option<Value>, key: &str) -> u64 {
    state
        .as_ref()
        .and_then(|value| value.get(key)?.as_u64())
        .unwrap_or_default()
}

fn duplicates(bodies: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    bodies
        .iter()
        .filter(|body| !seen.insert((*body).clone()))
        .cloned()
        .collect()
}

/// Drains to the end, reporting wakes and requests. `refused` are spread through
/// the spool but never last -- a spool whose final record is refused is held on
/// purpose and has its own tests (`copilot_push_held.rs`).
async fn drain_to_end(label: &str, count: usize, refused: &[usize]) -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::RejectContaining {
        needle: "POISON",
        status: 400,
    })
    .await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    fixture::write_spool(&spool, &spool_with(count, refused))?;
    let size = std::fs::metadata(&spool).context("sizing the spool")?.len();

    let mut wakes = 0;
    let mut previous = 0;
    while number(&checkpoint(&harness)?, "offset") < size && wakes < 60 {
        let output = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
        wakes += 1;
        let now = number(&checkpoint(&harness)?, "offset");
        assert!(
            now > previous,
            "{label}: wake {wakes} made no progress, offset stuck at {now} of {size}. stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        previous = now;
    }

    let state = checkpoint(&harness)?;
    let delivered = collector.accepted_log_bodies()?;
    let repeated = duplicates(&delivered);
    let requests = collector.request_count()?;
    println!(
        "{label}: {count} records / {} refused -> {wakes} wakes, {requests} requests \
         ({:.1}/wake), {} delivered, {} duplicates, discarded_total={}",
        refused.len(),
        requests as f64 / f64::from(wakes),
        delivered.len(),
        repeated.len(),
        number(&state, "discarded_total"),
    );

    assert_eq!(number(&state, "offset"), size, "{label}: never drained");
    assert!(
        repeated.is_empty(),
        "{label}: {} record(s) delivered twice: {:?}",
        repeated.len(),
        repeated.iter().take(5).collect::<Vec<_>>()
    );
    // Conservation: every record either arrived or was counted as lost.
    let accounted = delivered.len() as u64 + number(&state, "discarded_total");
    assert_eq!(
        accounted, count as u64,
        "{label}: {count} records went in, {accounted} accounted for -- the offset passed \
         records that were neither delivered nor counted"
    );
    assert_eq!(
        number(&state, "discarded_total"),
        refused.len() as u64,
        "{label}: exactly the refused records may be discarded, and only after two wakes each"
    );
    Ok(())
}

#[tokio::test]
async fn two_thousand_six_hundred_records_with_ten_refused_never_duplicate() -> Result<()> {
    let refused: Vec<usize> = (0..10).map(|n| 137 + n * 233).collect();
    drain_to_end("2600/10", 2600, &refused).await
}

#[tokio::test]
async fn five_hundred_and_twelve_records_with_twelve_refused_never_duplicate() -> Result<()> {
    let refused: Vec<usize> = (0..12).map(|n| 17 + n * 37).collect();
    drain_to_end("512/12", 512, &refused).await
}

/// The quarantine rule, measured rather than assumed: wake 1 may discard
/// nothing, however obviously bad the record looks.
#[tokio::test]
async fn no_record_is_ever_discarded_on_one_wakes_evidence() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::RejectContaining {
        needle: "POISON",
        status: 400,
    })
    .await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    fixture::write_spool(&spool, &spool_with(64, &[7, 23, 41]))?;

    fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let after_first = checkpoint(&harness)?;

    println!(
        "quarantine: after wake 1 discarded_total={}",
        number(&after_first, "discarded_total")
    );
    assert_eq!(
        number(&after_first, "discarded_total"),
        0,
        "one wake's 400 is evidence, not a verdict: {after_first:?}"
    );
    assert!(
        number(&after_first, "offset") > 0,
        "and the records before it must still have been delivered: {after_first:?}"
    );
    Ok(())
}
