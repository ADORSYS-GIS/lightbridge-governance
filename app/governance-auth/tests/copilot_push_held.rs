//! The one stall that does not clear on the next wake, end to end.
//!
//! A record the collector refuses on its own is given up on only once the
//! collector has been shown to accept *something* -- and when the refused
//! record is the **last** one in the spool there is nothing after it to show
//! that with. So it is held, for ever: every wake exits 1, and the only thing
//! that resolves it is Copilot appending another record.
//!
//! That behaviour is deliberate and is not what this file argues with. What it
//! pins is that the developer can *find out*. Round 2 deleted the doc sentence
//! describing it and replaced it with nothing, leaving the behaviour to exist
//! only as a comment in `export/isolate.rs`, while `status` rendered it as an
//! ordinary yellow "N bytes pending ... run `governance-auth copilot push`" --
//! advice whose only effect is to reproduce the same failing wake.

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

/// A good record then a permanently refused one, so the refused record is last
/// and the probe that would resolve it has nothing to offer.
async fn stall(
    harness: &Harness,
    collector: &MockCollector,
    spool: &std::path::Path,
) -> Result<()> {
    fixture::write_spool(
        spool,
        &[fixture::log_line(), fixture::marked_log_line("ALWAYS-BAD")],
    )?;
    // Two wakes: the first refusal is only evidence, the second is the verdict
    // -- and the verdict here is "held", because nothing proves the collector
    // works.
    for _ in 0..2 {
        fixture::push(harness, &collector.base_url, spool, &[]).await?;
    }
    Ok(())
}

/// The stall is real, and it is recorded rather than merely happening.
#[tokio::test]
async fn a_spool_whose_last_record_is_refused_is_held_and_says_so() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::RejectContaining {
        needle: "ALWAYS-BAD",
        status: 400,
    })
    .await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    stall(&harness, &collector, &spool).await?;

    let third = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let stderr = String::from_utf8_lossy(&third.stderr).into_owned();

    assert!(
        !third.status.success(),
        "a wake that resolved nothing must exit non-zero: {stderr}"
    );
    let state = checkpoint(&harness)?;
    assert_eq!(
        state
            .as_ref()
            .and_then(|value| value.get("discarded_total")?.as_u64())
            .unwrap_or_default(),
        0,
        "nothing proved the collector works, so nothing may be discarded: {state:?}"
    );
    assert!(
        state
            .as_ref()
            .and_then(|value| value.get("held_since_unix")?.as_u64())
            .is_some(),
        "the stall has to be recorded, or `status` cannot tell it from a backlog: {state:?}"
    );
    assert!(
        stderr.contains("LAST one in the spool") && stderr.contains("clears when Copilot writes"),
        "the run must explain what does and does not resolve this, got: {stderr}"
    );
    Ok(())
}

/// `status` still runs, and still exits 0, against a held drain.
///
/// ⚠️ It cannot be asserted on here beyond that. The spool row lives in the
/// table, and the table is deliberately gated on `console::user_attended_stderr`
/// -- with no TTY `status` prints only its one documented plain line, which is
/// what a subprocess test sees. Rendering the row is therefore pinned where it
/// can be: `dashboard::tests::spool` for the row itself and for the fact that
/// it reaches the table. What this covers is the join between them -- the
/// checkpoint field that `SpoolStatus::survey` reads to choose that row, which
/// the test above proves is written.
#[tokio::test]
async fn status_does_not_fall_over_on_a_held_drain() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::RejectContaining {
        needle: "ALWAYS-BAD",
        status: 400,
    })
    .await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    stall(&harness, &collector, &spool).await?;

    let status = harness
        .run(&[
            "status",
            "--copilot-spool-path",
            &spool.display().to_string(),
        ])
        .await?;
    let out = String::from_utf8_lossy(&status.stderr).into_owned();

    assert!(status.status.success(), "status must not fail: {out}");
    assert!(
        out.contains("session cached"),
        "the documented plain line is unchanged: {out}"
    );
    Ok(())
}

/// And it really does clear on a new record, which is the claim the row makes.
#[tokio::test]
async fn a_later_record_clears_the_hold_and_the_marker() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::RejectContaining {
        needle: "ALWAYS-BAD",
        status: 400,
    })
    .await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    stall(&harness, &collector, &spool).await?;

    // Copilot writes one more record. Now there is something to prove the
    // collector with, so the bad record is finally given up on.
    fixture::write_spool(
        &spool,
        &[
            fixture::log_line(),
            fixture::marked_log_line("ALWAYS-BAD"),
            fixture::marked_log_line("arrived-later"),
        ],
    )?;
    let size = std::fs::metadata(&spool).context("sizing the spool")?.len();
    let after = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;

    let state = checkpoint(&harness)?;
    assert!(
        after.status.success(),
        "the wake resolved everything it read: {}",
        String::from_utf8_lossy(&after.stderr)
    );
    assert_eq!(
        state
            .as_ref()
            .and_then(|v| v.get("held_since_unix")?.as_u64()),
        None,
        "the marker must clear, or `status` shows a stall that is over: {state:?}"
    );
    assert_eq!(
        state.as_ref().and_then(|v| v.get("offset")?.as_u64()),
        Some(size),
        "and the stream must not stop at the bad record's byte offset: {state:?}"
    );
    assert!(
        collector
            .accepted_log_bodies()?
            .iter()
            .any(|body| body == "arrived-later"),
        "the record that unblocked it must itself have been delivered"
    );
    Ok(())
}
