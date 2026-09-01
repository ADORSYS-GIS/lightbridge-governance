//! Data must never leave the spool without either reaching the collector or
//! being **recorded as lost**.
//!
//! Both tests here reproduce a way the drain used to consume bytes and still
//! report success:
//!
//! - a Copilot release renames the private fields the parser dispatches on, so
//!   every record classifies as unrecognised, both payloads come back empty,
//!   no POST is made -- and the checkpoint advances anyway. The dashboard then
//!   reads `pending == 0` and paints the row green.
//! - a run against a spool path that does not exist rewinds the checkpoint to
//!   0, so the next correct run re-exports the whole file.
//!
//! Neither is visible from a request count, which is why the assertions below
//! are on the **checkpoint file** and on what the run says about itself.

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

fn field(state: &Option<Value>, key: &str) -> Option<u64> {
    state.as_ref()?.get(key)?.as_u64()
}

/// THE silent-loss case. Three records the parser cannot place: nothing is
/// posted, so a request count proves nothing -- but the bytes are gone from
/// the drain's point of view the moment the offset moves past them.
#[tokio::test]
async fn records_the_parser_cannot_place_are_counted_as_lost_not_swallowed() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    fixture::write_spool(
        &spool,
        &[
            fixture::drifted_line(),
            fixture::drifted_line(),
            fixture::drifted_line(),
        ],
    )?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    let output = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    assert_eq!(
        collector.request_count()?,
        0,
        "the fixture is only meaningful if nothing was exportable"
    );
    let state = checkpoint(&harness)?;
    assert_eq!(
        field(&state, "discarded_total"),
        Some(3),
        "three records were consumed and never delivered; the checkpoint must say so rather than \
         advancing as though they had been. Got: {state:?}"
    );
    assert!(
        field(&state, "last_discard_unix").is_some(),
        "a loss with no timestamp cannot be aged out or explained later: {state:?}"
    );
    assert!(
        stderr.contains("discarded"),
        "the run must name the loss in its own words, got: {stderr}"
    );
    Ok(())
}

/// The third outcome the invariant has no room for: **delivered empty**.
///
/// Only `_body` is renamed here, so the record still parses and still looks
/// like a log line. It used to be exported with a timestamp, some attributes
/// and nothing else -- a request the collector answered 200 to, a run that
/// printed "Pushed 2 record(s)", and a green `status` row. Recorded as
/// delivered, and the content gone.
#[tokio::test]
async fn a_record_that_would_be_exported_empty_is_counted_as_lost_not_delivered() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    fixture::write_spool(
        &spool,
        &[fixture::body_renamed_line(), fixture::body_renamed_line()],
    )?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    let output = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    assert_eq!(
        collector.request_count()?,
        0,
        "an empty envelope is not a delivery; posting it is what made this look successful. \
         Payloads: {:?}",
        collector.payloads()?
    );
    let state = checkpoint(&harness)?;
    assert_eq!(
        field(&state, "discarded_total"),
        Some(2),
        "both records were consumed and carried nothing to the collector: {state:?}, stderr: \
         {stderr}"
    );
    assert!(
        !stderr.contains("Pushed"),
        "a run that delivered nothing must not report a push: {stderr}"
    );
    Ok(())
}

/// A run pointed at a path that does not exist must leave the checkpoint
/// exactly where it was. Rewinding to 0 makes the next *correct* run re-export
/// the entire spool, which is duplicated billing data at the collector.
#[tokio::test]
async fn a_missing_spool_never_rewinds_the_checkpoint() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    let first = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(
        first.status.success(),
        "first run failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let after_first = field(&checkpoint(&harness)?, "offset");
    assert!(
        after_first.unwrap_or_default() > 0,
        "the fixture needs a real offset to rewind from"
    );
    let requests_after_first = collector.request_count()?;

    // A typo'd path, an unmounted home, or simply running before VS Code has
    // recreated the file.
    let absent = harness.state_dir().join("no-such-spool.jsonl");
    let output = fixture::push(&harness, &collector.base_url, &absent, &[]).await?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    assert!(
        output.status.success(),
        "a developer who has not used Chat yet must not see a failing timer: {stderr}"
    );
    assert_eq!(
        field(&checkpoint(&harness)?, "offset"),
        after_first,
        "a spool that is not there says nothing about how far the real one was drained"
    );
    assert!(
        !stderr.contains("truncated or rotated"),
        "a file that does not exist was not rotated; that message sends the reader looking for a \
         rotation that never happened. Got: {stderr}"
    );

    // And the real spool is still considered drained, not re-exported.
    let third = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(third.status.success());
    assert_eq!(
        collector.request_count()?,
        requests_after_first,
        "the rewound checkpoint would re-export every record the collector already has"
    );
    Ok(())
}
