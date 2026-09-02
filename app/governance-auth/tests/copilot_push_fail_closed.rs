//! `copilot push` must fail closed: **no valid token means no data is
//! consumed.**
//!
//! This is the property the whole command is arranged around. A drain that
//! reads the spool, fails to authenticate, and then advances its checkpoint
//! anyway would delete a developer's telemetry permanently -- silently, and
//! only on the days the token happened to be bad. So each test here asserts
//! all four halves of "nothing was consumed": non-zero exit, the collector
//! saw zero requests, the checkpoint file is unchanged (or still absent), and
//! the spool is byte-for-byte what it was.

mod support;

use anyhow::{Context, Result};
use support::{
    copilot as fixture,
    harness::Harness,
    mock_collector::{Behavior, MockCollector},
};

#[tokio::test]
async fn no_cached_session_consumes_nothing() -> Result<()> {
    // `.invalid` never resolves (RFC 2606): if anything here reached the
    // network, it would fail rather than quietly succeed.
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    let before = std::fs::read(&spool).context("reading the seeded spool")?;

    let output = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;

    assert!(
        !output.status.success(),
        "an unauthenticated drain must exit non-zero"
    );
    assert_eq!(
        collector.request_count()?,
        0,
        "nothing may be exported without a token"
    );
    assert!(
        !fixture::checkpoint_path(&harness).exists(),
        "the checkpoint must not be created by a run that never authenticated"
    );
    assert_eq!(
        std::fs::read(&spool).context("re-reading the spool")?,
        before,
        "the spool must be left byte-identical"
    );
    Ok(())
}

/// A run with nothing to do still has to fail closed rather than exit 0.
///
/// ⚠️ What this test pins, measured rather than assumed -- an earlier comment
/// here claimed moving the auth block below the drain "makes exactly this test
/// fail, and no other". Both halves of that were false. Injected into the code
/// as it stood before this file's fixes:
///
/// - auth moved to just before the POSTs: **two** pre-existing tests failed --
///   this one and `dry_run_still_requires_a_valid_session`.
/// - auth moved to just after the drain: **none** failed. Every assertion here
///   was on a request count, a checkpoint file or the spool's bytes, and none
///   changes when the read happens first and the credential check second.
///
/// The read half is now pinned by
/// `the_spool_is_not_even_opened_before_authentication` below, the only test
/// that second injection reaches. This one covers the write half: no state may
/// be written by a run that never presented a credential.
#[tokio::test]
async fn an_empty_spool_still_authenticates_before_writing_any_state() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    std::fs::write(&spool, b"").context("emptying the spool")?;

    let output = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;

    assert!(
        !output.status.success(),
        "a run with nothing to do must still fail closed, not exit 0 unauthenticated"
    );
    assert!(
        !fixture::checkpoint_path(&harness).exists(),
        "no credential was ever presented, so no state may be written"
    );
    Ok(())
}

/// The ordering assertion that actually pins "**does not read the spool**".
///
/// Every other test here asserts on a request count, a checkpoint file or the
/// spool's bytes, and none of those changes when the read happens first and
/// authentication fails second -- the run still exits non-zero having exported
/// nothing. They pin the weaker "no export and no checkpoint write without a
/// token", and an auth block moved below the drain leaves them all green.
///
/// A directory as the spool path separates them. `File::open` succeeds on it
/// and the `read` fails with `EISDIR` -- for any user, root included, which a
/// mode-000 file would not. So a run that reads first dies complaining about
/// the path and a run that authenticates first dies complaining about the
/// credential, and only one of those can be true at a time.
#[tokio::test]
async fn the_spool_is_not_even_opened_before_authentication() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let unreadable = harness.state_dir().join("spool-that-is-a-directory");
    std::fs::create_dir_all(&unreadable).context("creating the unreadable spool path")?;

    let output = fixture::push(&harness, &collector.base_url, &unreadable, &[]).await?;
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(
        stderr.contains("without a valid session"),
        "the credential must be the reason this run stopped, got: {stderr}"
    );
    assert!(
        !stderr.to_lowercase().contains("is a directory"),
        "reaching an EISDIR means the spool was opened before the token was obtained: {stderr}"
    );
    Ok(())
}

/// The same property one step further in: a session exists but is expired and
/// has no refresh token, so the token step fails *after* the config resolved.
#[tokio::test]
async fn an_expired_session_that_cannot_refresh_consumes_nothing() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::expired_session(harness.issuer())?)?;

    // Plant a checkpoint so "unchanged" is a real assertion and not just
    // "absent", which the test above already covers.
    let checkpoint = fixture::checkpoint_path(&harness);
    std::fs::write(&checkpoint, br#"{"offset":7,"last_push_unix":null}"#)
        .context("planting a checkpoint")?;

    let output = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;

    assert!(!output.status.success());
    assert_eq!(collector.request_count()?, 0);
    assert_eq!(
        std::fs::read_to_string(&checkpoint).context("reading the checkpoint")?,
        r#"{"offset":7,"last_push_unix":null}"#,
        "a failed drain must not move the checkpoint"
    );
    Ok(())
}

/// `--dry-run` is held to the same bar deliberately. An offline preview would
/// be a second path that reads the spool without a credential, and "there is
/// exactly one such path and it starts with authentication" is a far easier
/// property to keep true. See `crate::copilot`'s module doc.
#[tokio::test]
async fn dry_run_still_requires_a_valid_session() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;

    let output = fixture::push(&harness, &collector.base_url, &spool, &["--dry-run"]).await?;

    assert!(
        !output.status.success(),
        "--dry-run must not be an offline read path for the spool"
    );
    assert_eq!(collector.request_count()?, 0);
    assert!(!fixture::checkpoint_path(&harness).exists());
    Ok(())
}

/// A collector that rejects the batch is the other half: authentication
/// succeeded, the transform succeeded, and the export did not. The bytes must
/// stay pending rather than being marked as delivered.
#[tokio::test]
async fn a_rejected_export_does_not_advance_the_checkpoint() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Reject(503)).await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    let output = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;

    assert!(!output.status.success());
    assert!(
        collector.request_count()? > 0,
        "this test is only meaningful if the export was actually attempted"
    );
    assert!(
        !fixture::checkpoint_path(&harness).exists(),
        "a rejected export must leave the bytes pending, not record them as sent"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("copilot_chat.tool.call"),
        "the payload must never be logged, got: {stderr}"
    );
    Ok(())
}
