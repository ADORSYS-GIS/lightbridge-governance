//! A backlog has to clear in a wake, and it may not clear by cutting corners.
//!
//! `copilot-push` reads at most 8 MiB per sweep -- a memory bound on the
//! `Vec<u8>` the spool is read into -- and that used to bound the whole *wake*.
//! Measured on a maintainer's desktop, 2026-09-02: a 164 MB spool draining
//! 8,385,060 bytes per wake at one wake per 300 s, so 27 KB/s and ~18 wakes to
//! catch up. `spool::reclaim` fires only at `size == offset`, so the spools
//! with the most to reclaim were the ones that could never present it.
//!
//! A wake now sweeps repeatedly, and these two tests are the halves of that
//! being true at once: it must clear a multi-sweep backlog, and it must not buy
//! the throughput by re-offering bytes it has already offered.
//!
//! ⚠️ Both spools are deliberately over 8 MiB. A cheaper fixture would pass
//! against a single-sweep wake and prove nothing.

mod support;

use anyhow::{Context, Result};
use serde_json::Value;
use support::{
    copilot as fixture,
    harness::Harness,
    mock_collector::{Behavior, MockCollector},
};

/// Mirrors `copilot::spool::MAX_READ`; `tests/` cannot reach `src/`. A drift
/// shows up as the sweep count disagreeing with stderr, which is the point.
const MAX_READ: usize = 8 * 1024 * 1024;

/// Padding per record, so a multi-sweep spool is a few hundred records rather
/// than tens of thousands. The marker is the record's `_body`, which is what
/// the collector reports back, so every record is identifiable end to end.
const PADDING: usize = 256 * 1024;

fn padded(marker: &str) -> String {
    let mut line = fixture::marked_log_line(marker);
    if let Some(object) = line.as_object_mut()
        && let Some(attributes) = object.get_mut("attributes")
        && let Some(attributes) = attributes.as_object_mut()
    {
        attributes.insert("filler".to_owned(), Value::String("x".repeat(PADDING)));
    }
    format!("{line}\n")
}

/// `count` padded records, and the number of sweeps an 8 MiB read cap needs to
/// get through them -- computed from the same arithmetic the drain does
/// (whole lines only), so the test says how many it expects rather than
/// accepting whatever it got.
fn spool_of(count: usize) -> (String, usize) {
    let body: String = (0..count)
        .map(|index| padded(&format!("rec-{index}")))
        .collect();
    let per_sweep = MAX_READ / padded("rec-0").len();
    (body, count.div_ceil(per_sweep))
}

fn checkpoint(harness: &Harness) -> Result<Value> {
    let path = fixture::checkpoint_path(harness);
    serde_json::from_slice(&std::fs::read(&path).context("reading the checkpoint")?)
        .context("parsing the checkpoint")
}

/// The headline. One wake, a spool several times the read cap, and at the end
/// of it every record is at the collector exactly once and the file is empty.
///
/// Falsified by capping the loop at one sweep (`break` unconditionally after
/// the first `sweep::once`): 32 of 90 records arrive, the offset sits at ~8 MiB
/// of ~23 MB, the file is never reclaimed, and the stderr assertion below
/// reports one sweep instead of three.
#[tokio::test]
async fn one_wake_drains_a_spool_several_times_the_read_cap() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    let count = 90;
    let (contents, sweeps) = spool_of(count);
    std::fs::write(&spool, &contents).context("writing the spool")?;
    let initial = contents.len();
    assert!(
        sweeps >= 3,
        "the fixture must need several sweeps, not {sweeps}"
    );

    let run = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
    assert!(run.status.success(), "the wake must succeed: {stderr}");

    let delivered = collector.accepted_log_bodies()?;
    let unique: std::collections::BTreeSet<&String> = delivered.iter().collect();
    println!(
        "backlog: {initial} bytes / {count} records -> 1 wake, {} delivered, {} unique",
        delivered.len(),
        unique.len(),
    );

    // Conservation, both directions: nothing skipped, nothing sent twice.
    assert_eq!(
        (unique.len(), delivered.len()),
        (count, count),
        "a sweep boundary skipped records or re-offered delivered ones"
    );
    assert!(
        stderr.contains(&format!("Drained {sweeps} sweeps")),
        "a backlogged wake has to be legible as one in the journal; expected {sweeps} sweeps in: \
         {stderr}"
    );
    // And the reclaim, which is the whole reason the throughput mattered: the
    // sweep that finishes the backlog is the first one ever to hold
    // `size == offset`, so it fires in this same wake.
    assert_eq!(
        std::fs::metadata(&spool).context("sizing")?.len(),
        0,
        "{initial} bytes were delivered in full and the file still holds them: {stderr}"
    );
    assert_eq!(
        checkpoint(&harness)?.get("offset").and_then(Value::as_u64),
        Some(0),
        "an offset into reclaimed bytes re-reads a file that has already been sent"
    );
    Ok(())
}

/// ⚠️ The corner the loop must not cut. A record the collector refuses on its
/// own may be discarded only after `REFUSALS_BEFORE_DISCARD` **separate wakes**
/// have refused it. A sweep that stops on such a record has already offered
/// every byte it read, so sweeping again inside the same wake would re-offer
/// it, take a second refusal, and discard a record on one wake's evidence.
///
/// Falsified by relaxing the loop's `!swept.complete` guard to "the offset did
/// not move": the second sweep re-reads from the poisoned record, refuses it
/// again, probes the record after it, and `discarded_total` reaches 1 on wake
/// one.
#[tokio::test]
async fn a_sweep_that_stops_short_ends_the_wake_rather_than_re_offering() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::RejectContaining {
        needle: "POISON",
        status: 400,
    })
    .await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    // Over the read cap, with the poison inside the FIRST sweep's range: the
    // records after it are what a second sweep would reach for.
    let count = 45;
    let (_, sweeps) = spool_of(count);
    assert!(
        sweeps >= 2,
        "the poison must leave a later sweep to be tempted by"
    );
    let contents: String = (0..count)
        .map(|index| padded(if index == 5 { "POISON" } else { "good" }))
        .collect();
    std::fs::write(&spool, &contents).context("writing the spool")?;

    let run = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
    let state = checkpoint(&harness)?;

    assert_eq!(
        state.get("discarded_total").and_then(Value::as_u64),
        Some(0),
        "one wake's 400 is evidence, not a verdict -- a second sweep must not supply the second \
         wake's opinion: {stderr}"
    );
    let refusals: Vec<u64> = state
        .get("quarantine")
        .and_then(Value::as_object)
        .map(|entries| {
            entries
                .values()
                .filter_map(|entry| entry.get("refusals")?.as_u64())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        refusals,
        vec![1],
        "exactly one record, refused exactly once, is what one wake may record: {state}"
    );
    assert!(
        state
            .get("offset")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            > 0,
        "and everything before the poison must still have been delivered: {state}"
    );
    assert_eq!(
        std::fs::metadata(&spool).context("sizing")?.len(),
        contents.len() as u64,
        "a spool with an undelivered record in it must not be reclaimed"
    );
    Ok(())
}
