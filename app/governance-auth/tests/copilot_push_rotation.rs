//! A replaced spool file must never be read as a continuation of the old one.
//!
//! `size < offset` is the only rotation the drain used to recognise, and it
//! answers the wrong question. VS Code recreates its outfile on restart; the
//! developer keeps working; five minutes later the **new** file is already
//! longer than the offset the **old** one left behind. The comparison is then
//! false, the drain seeks into the middle of a file it has never read, and
//! every record before that byte is skipped -- not delivered, not counted,
//! offset at the end. That is the one outcome `crate::copilot`'s stated
//! invariant ("delivered or recorded as lost") has no room for.
//!
//! Measured on the code this file was written against: a 2,700-byte spool
//! drained, replaced with a 5,412-byte one, and six brand-new records vanished
//! with `discarded_total` moving by 1 -- the partial-line fragment at the
//! resume point, and nothing else.
//!
//! The spool grew 73 KB -> 315 KB in six minutes of ordinary use on the machine
//! this was measured on, so "the new file outgrew the old offset inside one
//! timer window" is the ordinary case, not a corner one.

mod support;

use anyhow::{Context, Result};
use serde_json::Value;
use support::{
    copilot as fixture,
    harness::Harness,
    mock_collector::{Behavior, MockCollector},
};

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

fn field(state: &Option<Value>, key: &str) -> u64 {
    state
        .as_ref()
        .and_then(|value| value.get(key)?.as_u64())
        .unwrap_or_default()
}

/// Deliberately **variable length**. With fixed-length records the stale
/// offset lands exactly on a line boundary, so the resumed drain skips whole
/// records and counts nothing at all -- a cleaner defect, but one that makes
/// the `discarded_total` assertion below vacuous. The padding puts the resume
/// point in the middle of a record, which is what a real spool does and what
/// the gate measured (`discarded_total=1`, the fragment, and nothing else).
fn marker(prefix: &str, index: usize) -> String {
    format!("{prefix}{index:02}{}", "-pad".repeat(index))
}

fn marked(prefix: &str, count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| fixture::marked_log_line(&marker(prefix, index)))
        .collect()
}

fn missing(delivered: &[String], prefix: &str, count: usize) -> Vec<String> {
    (0..count)
        .map(|index| marker(prefix, index))
        .filter(|marker| !delivered.contains(marker))
        .collect()
}

/// THE regression test. A brand-new spool file that has already grown past the
/// old offset must be recognised as a new file, not resumed into.
#[tokio::test]
async fn a_rotation_that_outgrew_the_old_offset_is_still_a_rotation() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    // A longer prefix than the replacement's, so the stale offset cannot land
    // on a line boundary of the new file -- see `marker`.
    fixture::write_spool(&spool, &marked("stale", 6))?;
    let first = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(
        first.status.success(),
        "the fixture needs a clean first drain: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let old_offset = field(&checkpoint(&harness)?, "offset");
    assert!(
        old_offset > 0,
        "the fixture needs a real offset to skip past"
    );

    // VS Code restarted and recreated its outfile. By the time the five-minute
    // timer next fires, an active developer has already written more into the
    // new file than the old offset counted.
    std::fs::remove_file(&spool).context("removing the old spool")?;
    fixture::write_spool(&spool, &marked("new", 12))?;
    let size = std::fs::metadata(&spool).context("sizing the spool")?.len();
    assert!(
        size > old_offset,
        "this fixture only reproduces the defect when the NEW file has outgrown the OLD offset \
         ({size} vs {old_offset}); `size < offset` would otherwise catch it"
    );

    let second = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let stderr = String::from_utf8_lossy(&second.stderr).into_owned();
    let delivered = collector.accepted_log_bodies()?;
    let lost = missing(&delivered, "new", 12);
    assert!(
        lost.is_empty(),
        "{} brand-new record(s) were never delivered and never counted -- the offset was carried \
         over onto a file it was never measured against: {lost:?}. stderr: {stderr}",
        lost.len()
    );
    let state = checkpoint(&harness)?;
    assert_eq!(
        field(&state, "discarded_total"),
        0,
        "nothing here is unreadable; a non-zero count means bytes were consumed as a partial-line \
         fragment at a resume point in the middle of a file: {state:?}"
    );
    Ok(())
}

/// The reverse risk, and the reason "field absent" may not mean "mismatch": a
/// checkpoint written before file identity existed carries none. Treating that
/// as a rotation would re-export every developer's whole spool on upgrade --
/// duplicated usage data, caused by a version bump.
#[tokio::test]
async fn a_checkpoint_without_a_recorded_identity_is_not_read_as_a_rotation() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    fixture::write_spool(&spool, &marked("first", 6))?;
    let first = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(first.status.success(), "the fixture needs a clean drain");

    // Exactly what an older build left behind: the offsets, and nothing that
    // says which file they were measured against.
    let path = fixture::checkpoint_path(&harness);
    let mut state: serde_json::Map<String, Value> =
        serde_json::from_slice(&std::fs::read(&path).context("reading the checkpoint")?)
            .context("parsing the checkpoint")?;
    let pre_upgrade: Vec<String> = state
        .keys()
        .filter(|key| key.starts_with("spool") || key.contains("identity"))
        .cloned()
        .collect();
    for key in pre_upgrade {
        state.remove(&key);
    }
    std::fs::write(&path, Value::Object(state).to_string()).context("planting the checkpoint")?;

    let delivered_before = collector.accepted_log_bodies()?.len();
    let second = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(
        second.status.success(),
        "an upgrade must not turn into a failing wake: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        collector.accepted_log_bodies()?.len(),
        delivered_before,
        "the first run after an upgrade re-exported the whole spool: an absent identity field \
         says nothing about the file, so it must be adopted rather than read as a mismatch"
    );
    Ok(())
}

/// Copy-truncate keeps the inode and resets the size, so the pre-existing
/// `size < offset` path is the one that has to answer it. It must keep working.
#[tokio::test]
async fn a_spool_truncated_in_place_still_restarts_at_zero() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    fixture::write_spool(&spool, &marked("before", 8))?;
    let first = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(first.status.success(), "the fixture needs a clean drain");

    // Same file, same inode, fewer bytes -- `std::fs::write` truncates in place.
    fixture::write_spool(&spool, &marked("after", 2))?;

    let second = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let stderr = String::from_utf8_lossy(&second.stderr).into_owned();
    assert!(
        stderr.contains("truncated or rotated"),
        "a restart the developer cannot see makes a duplicated push mysterious: {stderr}"
    );
    let lost = missing(&collector.accepted_log_bodies()?, "after", 2);
    assert!(lost.is_empty(), "the whole new file must be read: {lost:?}");
    Ok(())
}
