//! A 400 is not always a property of the payload.
//!
//! The drain gives up on a record the collector refuses on its own. That rule
//! assumed HTTP 400 is a deterministic function of what was posted -- which is
//! true of a collector and false of anything sitting in front of one. A WAF, a
//! reverse proxy, a rate limiter answering the wrong status, an upstream
//! restart: all of them return 400 for reasons that have nothing to do with
//! the record, and none of them return it twice for the same reason.
//!
//! Measured against a gateway answering 400 for roughly half of all requests,
//! one round in twelve permanently discarded four **valid** records and exited
//! 0. The record was gone, `status` said so, and no retry would ever bring it
//! back -- for a transport blip.
//!
//! So the drain now needs the same record refused on its own across separate
//! wakes. The two tests below are the two sides of that: a blip must cost
//! nothing, and a genuinely bad record must still be cleared.

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

/// THE regression test for the wrongly-discarded record. The collector refuses
/// one record on wake 1 for a reason that has gone away by wake 2. Nothing may
/// be discarded, and the record must arrive.
#[tokio::test]
async fn a_record_refused_once_and_accepted_next_wake_is_never_discarded() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::RejectContaining {
        needle: "BLIP-RECORD",
        status: 400,
    })
    .await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    fixture::write_spool(
        &spool,
        &[
            fixture::log_line(),
            fixture::marked_log_line("BLIP-RECORD"),
            fixture::log_line(),
        ],
    )?;
    let size = std::fs::metadata(&spool).context("sizing the spool")?.len();

    let first = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let stderr = String::from_utf8_lossy(&first.stderr).into_owned();
    assert_eq!(
        field(&checkpoint(&harness)?, "discarded_total"),
        0,
        "one wake's 400 came from a proxy, not from this record. stderr: {stderr}"
    );
    assert!(
        stderr.contains("held"),
        "the run must say the record is held rather than lost, got: {stderr}"
    );

    // The blip is over.
    collector.set_behavior(Behavior::Accept)?;
    let second = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(
        second.status.success(),
        "the second wake had nothing to refuse: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let state = checkpoint(&harness)?;
    assert_eq!(
        field(&state, "discarded_total"),
        0,
        "a record the collector went on to accept was never lost: {state:?}"
    );
    assert_eq!(
        field(&state, "offset"),
        size,
        "and the whole spool drained: {state:?}"
    );
    assert!(
        collector
            .accepted_log_bodies()?
            .iter()
            .any(|body| body == "BLIP-RECORD"),
        "the held record must actually have been delivered, got: {:?}",
        collector.accepted_log_bodies()?
    );
    Ok(())
}

/// A collector that worked this morning and refuses everything now must still
/// discard nothing.
///
/// ⚠️ This is the case a cheap answer gets wrong. "Has the collector accepted
/// anything?" is trivially answerable from the checkpoint's own
/// `last_push_unix` -- and that answer is stale in exactly the situation the
/// rule exists for. With it, a five-minute config error empties the spool one
/// record at a time for as long as the window lasts. The proof is therefore
/// obtained live, by offering the next record on its own, and a collector
/// refusing everything refuses that too.
#[tokio::test]
async fn a_collector_that_starts_refusing_everything_still_discards_nothing() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    fixture::write_spool(&spool, &[fixture::log_line(), fixture::log_line()])?;

    let healthy = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(healthy.status.success(), "the fixture needs a real push");
    let settled = field(&checkpoint(&harness)?, "offset");
    assert!(settled > 0, "and it must have moved the offset");

    // Someone breaks the collector. Four more records arrive.
    collector.set_behavior(Behavior::Reject(400))?;
    fixture::write_spool(
        &spool,
        &[
            fixture::log_line(),
            fixture::log_line(),
            fixture::marked_log_line("a"),
            fixture::marked_log_line("b"),
            fixture::marked_log_line("c"),
            fixture::marked_log_line("d"),
        ],
    )?;

    for wake in 1..=5 {
        let output = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
        assert!(!output.status.success(), "wake {wake} delivered nothing");
        let state = checkpoint(&harness)?;
        assert_eq!(
            field(&state, "discarded_total"),
            0,
            "wake {wake} gave up on a record while the collector was refusing everything -- that \
             is a configuration fault, and answering it by discarding turns five minutes of \
             misconfiguration into permanent loss: {state:?}"
        );
        assert_eq!(
            field(&state, "offset"),
            settled,
            "and nothing may advance past bytes the collector never took: {state:?}"
        );
    }
    Ok(())
}

/// The other side: a record the collector refuses *every* time is still given
/// up on, so holding is a delay and never a new poison pill.
#[tokio::test]
async fn a_record_refused_on_two_separate_wakes_is_given_up_on() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::RejectContaining {
        needle: "ALWAYS-BAD",
        status: 422,
    })
    .await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    fixture::write_spool(
        &spool,
        &[
            fixture::log_line(),
            fixture::marked_log_line("ALWAYS-BAD"),
            fixture::log_line(),
        ],
    )?;
    let size = std::fs::metadata(&spool).context("sizing the spool")?.len();

    let first = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(
        !first.status.success(),
        "wake 1 could not resolve the record and must say so rather than exiting 0"
    );
    // Wake 2 has the second refusal it needed, gives up on the record and gets
    // the rest of the batch through -- so it is a *successful* run.
    let second = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(
        second.status.success(),
        "wake 2 resolved everything it read: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let state = checkpoint(&harness)?;
    assert_eq!(
        field(&state, "discarded_total"),
        1,
        "two separate wakes refused it on its own; that is the evidence the rule asks for: \
         {state:?}"
    );
    assert_eq!(
        field(&state, "offset"),
        size,
        "and the stream must not stop at its byte offset: {state:?}"
    );
    Ok(())
}
