//! Re-running `copilot-push` with no new data must push nothing and change
//! nothing.
//!
//! This is the "writes are idempotent" rule from `AGENTS.md` applied to a
//! client-side drain: the collector charges what it is told, so a re-run that
//! re-exports already-delivered records is duplicated usage data. The
//! assertion is on the **collector's request count**, not on a log line --
//! that is the only thing that proves nothing went out.

mod support;

use anyhow::{Context, Result};
use support::{
    copilot as fixture,
    harness::Harness,
    mock_collector::{Behavior, MockCollector},
};

/// Reads the checkpoint's `offset`. `None` when there is no checkpoint yet.
fn offset(harness: &Harness) -> Result<Option<u64>> {
    let path = fixture::checkpoint_path(harness);
    if !path.exists() {
        return Ok(None);
    }
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).context("reading the checkpoint")?)
            .context("parsing the checkpoint")?;
    Ok(value.get("offset").and_then(serde_json::Value::as_u64))
}

#[tokio::test]
async fn a_second_run_with_no_new_data_pushes_nothing() -> Result<()> {
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
    let after_first = collector.request_count()?;
    assert_eq!(
        collector.paths()?,
        vec!["/v1/metrics".to_owned(), "/v1/logs".to_owned()],
        "one metrics line and one log line must land on their own signal paths"
    );
    assert!(
        collector.every_request_authenticated()?,
        "every export must carry a bearer"
    );
    let checkpoint_after_first = offset(&harness)?;
    assert!(
        checkpoint_after_first.unwrap_or_default() > 0,
        "a successful push must record where it got to"
    );

    // THE assertion. Same spool, same checkpoint, nothing appended.
    let second = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(
        second.status.success(),
        "a no-op run is a success, not an error: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        collector.request_count()?,
        after_first,
        "re-running with no new data must send the collector nothing at all"
    );
    assert_eq!(
        offset(&harness)?,
        checkpoint_after_first,
        "and must not move the checkpoint"
    );
    Ok(())
}

#[tokio::test]
async fn only_records_appended_since_the_last_run_are_pushed() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let after_first = collector.request_count()?;

    // VS Code appends one more log record.
    fixture::write_spool(
        &spool,
        &[
            fixture::metrics_line(),
            fixture::log_line(),
            fixture::log_line(),
        ],
    )?;
    let second = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(second.status.success());

    let new_requests: Vec<String> = collector
        .paths()?
        .split_off(after_first)
        .into_iter()
        .collect();
    assert_eq!(
        new_requests,
        vec!["/v1/logs".to_owned()],
        "only the appended log record is new, so no metrics export should happen"
    );

    let payloads = collector.payloads()?;
    let (_, last) = payloads.last().context("no payload was captured")?;
    let records = last
        .pointer("/resourceLogs/0/scopeLogs/0/logRecords")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();
    assert_eq!(records, 1, "the already-pushed record must not be re-sent");
    Ok(())
}

/// The spool is append-only from this command's point of view: `AGENTS.md`'s
/// "never rewrite the file a live writer holds open" is enforced by never
/// opening it for writing at all.
#[tokio::test]
async fn a_successful_push_never_writes_to_the_spool() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let before = std::fs::read(&spool).context("reading the seeded spool")?;

    let output = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    assert!(output.status.success());

    assert_eq!(
        std::fs::read(&spool).context("re-reading the spool")?,
        before,
        "the drain must not truncate or rewrite a file VS Code holds open"
    );
    Ok(())
}

/// A rotation under a stale checkpoint restarts at zero and says so, rather
/// than seeking past the end of the new file and silently exporting nothing
/// forever.
#[tokio::test]
async fn a_rotated_spool_restarts_and_reports_it() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    fixture::push(&harness, &collector.base_url, &spool, &[]).await?;
    let after_first = collector.request_count()?;

    // Same path, fewer bytes: Copilot rolled its outfile.
    fixture::write_spool(&spool, &[fixture::log_line()])?;
    let second = fixture::push(&harness, &collector.base_url, &spool, &[]).await?;

    assert!(second.status.success());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("truncated or rotated"),
        "a restart must be explicable from the logs, got: {stderr}"
    );
    assert!(
        collector.request_count()? > after_first,
        "the new file's contents must actually be exported"
    );
    Ok(())
}

/// `--dry-run` with a valid session parses and reports, but leaves the
/// checkpoint alone and posts nothing.
#[tokio::test]
async fn dry_run_reports_without_consuming() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;
    let spool = fixture::seed_spool(&harness)?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;

    let output = fixture::push(&harness, &collector.base_url, &spool, &["--dry-run"]).await?;

    assert!(output.status.success());
    assert_eq!(collector.request_count()?, 0, "--dry-run must post nothing");
    assert_eq!(offset(&harness)?, None, "--dry-run must not checkpoint");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("1 metric record(s), 1 log record(s)"),
        "it must still report what it found, got: {stderr}"
    );
    Ok(())
}
