//! What happens when the collector takes *some* of what a run offers.
//!
//! Two failure shapes, both reproduced against a real listener:
//!
//! - **Partial export.** Metrics are accepted, logs are rejected. With one
//!   shared offset the accepted metrics batch is rebuilt and re-posted on
//!   every later wake, forever -- duplicated usage data that nothing in the
//!   drain can notice, because from its side the run simply failed.
//! - **A permanently rejected record.** A validating collector 400s one bad
//!   record, the whole batch dies with it, and the same bytes rebuild the same
//!   rejected payload every wake. Nothing splits, quarantines or advances past
//!   it, so the stream stops at that byte offset for good.

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

fn count(paths: &[String], path: &str) -> usize {
    paths.iter().filter(|seen| *seen == path).count()
}

/// Metrics land, logs do not. The metrics half must not be offered again.
#[tokio::test]
async fn an_accepted_signal_is_not_re_sent_when_the_other_one_fails() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::RejectPath {
        path: "/v1/logs",
        status: 503,
    })
    .await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    let first = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(
        !first.status.success(),
        "a rejected logs export is a failed run"
    );
    assert_eq!(
        count(&collector.paths()?, "/v1/metrics"),
        1,
        "the fixture needs the metrics half to have been accepted"
    );

    let second = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(!second.status.success(), "logs are still being rejected");

    assert_eq!(
        count(&collector.paths()?, "/v1/metrics"),
        1,
        "the collector already accepted these metrics; re-posting them is duplicated usage data \
         and nothing on this side would ever notice"
    );
    assert!(
        count(&collector.paths()?, "/v1/logs") >= 2,
        "the rejected half must keep being retried"
    );
    Ok(())
}

/// One record the collector will never accept must not stop every record
/// behind it. The bytes may be given up on -- but only after the collector has
/// demonstrably taken others from the same batch, only once **two separate
/// wakes** have each refused that record on its own, and only with the loss
/// recorded.
///
/// ⚠️ The second-wake requirement is not padding, and this test used to assert
/// the opposite (one wake, one discard). An audit against a gateway answering
/// 400 for reasons of its own -- a WAF, a proxy, an upstream hiccup -- had one
/// round in twelve permanently discard four *valid* records and exit 0,
/// because a single 400 was read as a property of the payload. So the
/// assertions below are strictly stronger than the one they replace: nothing
/// may be discarded on wake 1, and the same end state must still be reached.
#[tokio::test]
async fn a_permanently_rejected_record_does_not_block_the_stream_forever() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::RejectContaining {
        needle: "POISON-RECORD",
        status: 400,
    })
    .await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    fixture::write_spool(
        &spool,
        &[
            fixture::log_line(),
            fixture::marked_log_line("POISON-RECORD"),
            fixture::log_line(),
        ],
    )?;
    let size = std::fs::metadata(&spool).context("sizing the spool")?.len();

    let first = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let after_first = checkpoint(&harness)?;
    assert_eq!(
        after_first
            .as_ref()
            .and_then(|s| s.get("discarded_total")?.as_u64())
            .unwrap_or_default(),
        0,
        "one wake's 400 is not evidence that a record is bad: {after_first:?}, stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        after_first
            .as_ref()
            .and_then(|s| s.get("offset")?.as_u64())
            .unwrap_or_default()
            > 0,
        "the records before it were delivered, so the offset must have moved past them: \
         {after_first:?}"
    );

    let output = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    let state = checkpoint(&harness)?;
    assert_eq!(
        state.as_ref().and_then(|s| s.get("offset")?.as_u64()),
        Some(size),
        "the drain must get past one unacceptable record, or the stream stops here for good. \
         Got: {state:?}, stderr: {stderr}"
    );
    assert_eq!(
        state
            .as_ref()
            .and_then(|s| s.get("discarded_total")?.as_u64()),
        Some(1),
        "and it must be recorded as lost, not as delivered: {state:?}"
    );

    // The two good records really did arrive -- "advance past it" must not
    // mean "give up on the whole batch".
    let delivered: usize = collector
        .payloads()?
        .iter()
        .filter_map(|(path, payload)| {
            (path == "/v1/logs")
                .then(|| {
                    payload
                        .pointer("/resourceLogs/0/scopeLogs/0/logRecords")?
                        .as_array()
                })
                .flatten()
                .map(Vec::len)
        })
        .max()
        .unwrap_or_default();
    assert!(
        delivered >= 1,
        "the acceptable records must still have been exported: {stderr}"
    );
    Ok(())
}

/// The guard on the rule above: a collector that rejects *everything* is a
/// misconfiguration, not a batch of bad records. Discarding record by record
/// there would empty the spool into the void.
#[tokio::test]
async fn a_collector_that_rejects_everything_discards_nothing() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Reject(400)).await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    let output = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;

    assert!(!output.status.success());
    let state = checkpoint(&harness)?;
    assert_eq!(
        state
            .as_ref()
            .and_then(|s| s.get("discarded_total")?.as_u64())
            .unwrap_or_default(),
        0,
        "nothing was proved bad about these records: {state:?}"
    );
    Ok(())
}
