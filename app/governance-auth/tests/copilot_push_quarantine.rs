//! A record the collector refuses *every* time must still be cleared.
//!
//! The sibling side of `copilot_push_flaky.rs`. That file pins the rule that a
//! transport blip costs nothing; this one pins the other half, because a rule
//! that never gives up is just the poison pill under a kinder name: a record
//! refused on its own across two separate wakes is discarded, and the stream
//! moves past it.
//!
//! Split from `copilot_push_flaky.rs`, which had grown past the 200-LoC gate.
mod support;

use anyhow::{Context, Result};
use support::{
    checkpoint, copilot as fixture,
    harness::Harness,
    mock_collector::{Behavior, MockCollector},
};

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

    let state = checkpoint::checkpoint(&harness)?;
    assert_eq!(
        checkpoint::field(&state, "discarded_total").unwrap_or_default(),
        1,
        "two separate wakes refused it on its own; that is the evidence the rule asks for: \
         {state:?}"
    );
    assert_eq!(
        checkpoint::field(&state, "offset").unwrap_or_default(),
        size,
        "and the stream must not stop at its byte offset: {state:?}"
    );
    Ok(())
}
