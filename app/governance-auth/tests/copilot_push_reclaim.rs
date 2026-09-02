//! The spool must stop growing, and must not lose a record on its way to
//! stopping.
//!
//! Nothing bounded this file. It was measured at 73 KB -> 315 KB in six
//! minutes of ordinary use, and reached **12 MB in a few hours** on the machine
//! that reported #230/#241, still climbing. The reason given for not
//! truncating it -- that VS Code's next append would land at the old offset and
//! leave a zero-filled hole -- was measured false on 2026-09-02: those
//! descriptors are `O_APPEND`, so the append lands at 0. `spool::reclaim`
//! carries the measurement.
//!
//! What is left to protect is conservation. The reclaim destroys bytes rather
//! than advancing over them, so its whole correctness is one precondition:
//! `size == offset`, exactly. These run the real binary against a real spool
//! over the real threshold, once where that holds and once where it does not.

mod support;

use anyhow::{Context, Result};
use serde_json::Value;
use support::{
    copilot as fixture,
    harness::Harness,
    mock_collector::{Behavior, MockCollector},
};

/// Mirrors `copilot::spool::reclaim::RECLAIM_ABOVE`. Re-stated because
/// `tests/` cannot reach `src/`; a drift shows up as the first test below
/// finding an unreclaimed spool, which is the point.
const RECLAIM_ABOVE: u64 = 1024 * 1024;

fn checkpoint(harness: &Harness) -> Result<Value> {
    let path = fixture::checkpoint_path(harness);
    serde_json::from_slice(&std::fs::read(&path).context("reading the checkpoint")?)
        .context("parsing the checkpoint")
}

/// A spool comfortably over the threshold, whose last record carries `marker`
/// so a delivery assertion can name it.
fn oversized(marker: &str) -> String {
    let one = format!("{}\n", fixture::log_line());
    let count = (RECLAIM_ABOVE as usize / one.len()).saturating_add(2);
    let mut spool = one.repeat(count);
    spool.push_str(&format!("{}\n", fixture::marked_log_line(marker)));
    spool
}

/// The whole point: an oversized spool that the collector took in full goes
/// back to zero bytes, and the checkpoint goes with it.
#[tokio::test]
async fn an_oversized_spool_that_was_fully_delivered_is_reclaimed() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    std::fs::write(&spool, oversized("last-record")).context("writing the spool")?;
    let before = std::fs::metadata(&spool).context("sizing")?.len();
    assert!(before > RECLAIM_ABOVE, "the fixture must be over the bar");

    let run = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
    assert!(run.status.success(), "the wake must succeed: {stderr}");

    assert!(
        collector
            .accepted_log_bodies()?
            .iter()
            .any(|body| body == "last-record"),
        "reclaiming a spool whose records were not delivered would be the defect, not the fix"
    );
    let after = std::fs::metadata(&spool).context("sizing")?.len();
    assert_eq!(
        after, 0,
        "{before} bytes were delivered in full and the file still holds them; nothing bounds it"
    );
    assert!(
        stderr.contains("Reclaimed"),
        "a file that empties itself with no explanation is worse than one that grows: {stderr}"
    );
    let state = checkpoint(&harness)?;
    assert_eq!(
        state.get("offset").and_then(Value::as_u64),
        Some(0),
        "an offset into bytes that no longer exist re-reads a file it has already sent: {state}"
    );
    Ok(())
}

/// ⚠️ The conservation case. Copilot appends while the wake is posting -- the
/// ordinary thing for a machine somebody is using -- so the spool is over the
/// threshold and *not* caught up. The tail belongs to nobody yet: it has not
/// been delivered and it has not been counted, and truncating it away is the
/// one outcome the invariant has no room for.
///
/// Falsified by deleting the `size != delivered` guard in
/// `spool::reclaim::maybe`: the spool empties, the marker never reaches the
/// collector on this wake or any later one, and `discarded_total` stays 0.
#[tokio::test]
async fn a_record_written_during_the_wake_survives_the_reclaim() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    // The drain reads whole lines only, so a record still being written is
    // exactly a spool whose size runs past the offset the wake will reach.
    let mut contents = oversized("delivered-this-wake");
    contents.push_str("{\"hrTime\":[1788191912,6");
    std::fs::write(&spool, &contents).context("writing the spool")?;

    let first = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&spool)
            .context("re-reading the spool")?
            .len(),
        contents.len(),
        "the spool was reclaimed with an undelivered record in it"
    );

    // And the fragment is still a record: Copilot finishes the line, and the
    // next wake delivers it rather than finding it gone.
    let completed = format!(
        "{contents}13000000],\"resource\":{{\"_rawAttributes\":[]}},\"_body\":\"finished-later\"}}\n"
    );
    std::fs::write(&spool, completed).context("completing the record")?;
    let second = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let delivered = collector.accepted_log_bodies()?;
    assert!(
        delivered.iter().any(|body| body == "finished-later"),
        "the half-written record was consumed rather than left for the wake that could read it"
    );
    Ok(())
}
