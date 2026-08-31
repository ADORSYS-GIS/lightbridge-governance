//! Two `copilot-push` runs at once must not export the same record twice.
//!
//! This is not hypothetical: the `status` dashboard tells the developer to run
//! `governance-auth copilot-push` by hand, and the 5-minute timer that also
//! runs it has no idea. Read -> drain -> POST -> write-checkpoint is a
//! read-modify-write over one file; without a lock across the whole of it,
//! every concurrent run reads the same offset and ships the same bytes.
//!
//! The session lock in `cache::FileLock` does not cover this -- it guards the
//! session file and is dropped the moment `current_session` returns, long
//! before the spool is opened.

mod support;

use anyhow::{Context, Result};
use support::{
    copilot as fixture,
    harness::Harness,
    mock_collector::{Behavior, MockCollector},
};

#[tokio::test]
async fn three_concurrent_runs_export_each_record_once() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    // Slow on purpose: the unguarded window is read-checkpoint -> POST ->
    // write-checkpoint, and against an instant collector it is narrow enough
    // that three processes can miss each other by luck. See `Behavior`.
    let collector = MockCollector::start(Behavior::AcceptSlowly { millis: 400 }).await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    // One metrics record and one log record, so a correct outcome is exactly
    // one POST per signal no matter which process wins the race.
    let (first, second, third) = tokio::join!(
        fixture::push(&harness, &collector.base_url, &spool, &[]),
        fixture::push(&harness, &collector.base_url, &spool, &[]),
        fixture::push(&harness, &collector.base_url, &spool, &[]),
    );
    for output in [first?, second?, third?] {
        assert!(
            output.status.success(),
            "a run that loses the race is a no-op, not a failure: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let paths = collector.paths()?;
    assert_eq!(
        paths.len(),
        2,
        "each record must be exported exactly once across all three runs, got {paths:?}"
    );

    let checkpoint = fixture::checkpoint_path(&harness);
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&checkpoint).context("reading the checkpoint")?)
            .context("parsing the checkpoint")?;
    let size = std::fs::metadata(&spool).context("sizing the spool")?.len();
    assert_eq!(
        state.get("offset").and_then(serde_json::Value::as_u64),
        Some(size),
        "the winner's checkpoint must survive the losers' writes: {state}"
    );
    Ok(())
}
