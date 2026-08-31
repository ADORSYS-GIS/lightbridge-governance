//! Offset arithmetic: fresh file, partial consumption, no new data,
//! truncation/rotation, and a partial trailing line.
//!
//! These are the cases that decide whether re-running the drain is a no-op,
//! which is the idempotency property the whole checkpoint exists for.

use std::path::PathBuf;

use super::*;

/// A scratch directory removed on drop. Same hand-rolled shape as the test
/// harness under `tests/`, for the same reason: one call site, and the
/// cleanup is a single `remove_dir_all`.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "copilot-drain-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("creating the scratch directory");
        Self(path)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write(path: &std::path::Path, contents: &str) {
    std::fs::write(path, contents).expect("writing the spool fixture");
}

#[test]
fn a_fresh_spool_is_read_whole() {
    let dir = TempDir::new("fresh");
    let path = dir.file("spool.jsonl");
    let contents = format!("{}\n{}\n", metrics_line(), log_line());
    write(&path, &contents);

    let drained = spool::drain(&path, 0, None).expect("draining a fresh spool");

    assert_eq!(drained.lines.len(), 2);
    assert_eq!(drained.restarted, None);
    assert_eq!(drained.next_offset, contents.len() as u64);
    assert_eq!(drained.size, contents.len() as u64);
}

#[test]
fn re_running_with_no_new_data_pushes_nothing_and_moves_nothing() {
    let dir = TempDir::new("noop");
    let path = dir.file("spool.jsonl");
    let contents = format!("{}\n", log_line());
    write(&path, &contents);

    let first = spool::drain(&path, 0, None).expect("first drain");
    assert_eq!(first.lines.len(), 1);

    // THE idempotency assertion: same file, checkpoint from the first run.
    let second = spool::drain(&path, first.next_offset, None).expect("second drain");
    assert!(
        second.lines.is_empty(),
        "re-running with no new data must yield no records, got {:?}",
        second.lines
    );
    assert_eq!(
        second.next_offset, first.next_offset,
        "the offset must not move when nothing was read"
    );
    assert_eq!(
        second.restarted, None,
        "an unchanged file is not a rotation"
    );
    assert!(
        batch::build(&second.lines).metrics.is_none(),
        "and there must be nothing to post"
    );
}

#[test]
fn only_new_bytes_are_read_after_a_partial_consumption() {
    let dir = TempDir::new("partial");
    let path = dir.file("spool.jsonl");
    let first_line = format!("{}\n", log_line());
    write(&path, &first_line);

    let first = spool::drain(&path, 0, None).expect("first drain");

    let appended = format!("{first_line}{}\n", metrics_line());
    write(&path, &appended);

    let second = spool::drain(&path, first.next_offset, None).expect("second drain");
    assert_eq!(second.lines.len(), 1, "only the appended record");
    assert_eq!(record::classify(&json(&second)), record::Kind::Metrics);
    assert_eq!(second.next_offset, appended.len() as u64);
}

fn json(drained: &spool::Drain) -> Value {
    drained
        .lines
        .first()
        .and_then(|line| serde_json::from_str(&line.text).ok())
        .unwrap_or(Value::Null)
}

#[test]
fn a_truncated_spool_restarts_from_zero_and_says_so() {
    let dir = TempDir::new("truncate");
    let path = dir.file("spool.jsonl");
    let long = format!("{}\n{}\n", metrics_line(), log_line());
    write(&path, &long);
    let stale_offset = spool::drain(&path, 0, None)
        .expect("first drain")
        .next_offset;

    // VS Code rotated the outfile under us: same path, fewer bytes.
    let short = format!("{}\n", log_line());
    write(&path, &short);

    let after = spool::drain(&path, stale_offset, None).expect("drain after rotation");
    assert_eq!(
        after.restarted,
        Some(spool::Restart::Truncated),
        "a file shorter than the checkpoint must be reported as restarted, not silently skipped -- \
         and as TRUNCATED, because reporting a replacement sends the reader looking for a file \
         swap that never happened"
    );
    assert_eq!(after.lines.len(), 1, "the whole new file is read");
    assert_eq!(after.next_offset, short.len() as u64);
}

#[test]
fn a_missing_spool_is_not_an_error() {
    let dir = TempDir::new("missing");
    let drained =
        spool::drain(&dir.file("never-created.jsonl"), 0, None).expect("a missing spool is ok");
    assert!(drained.lines.is_empty());
    assert_eq!(drained.size, 0);
}

#[test]
fn a_partial_trailing_line_is_left_for_the_next_run() {
    let dir = TempDir::new("partial-line");
    let path = dir.file("spool.jsonl");
    // The writer is mid-append: the last record has no newline yet.
    let complete = format!("{}\n", log_line());
    write(&path, &format!("{complete}{{\"hrTime\":[1788191912"));

    let drained = spool::drain(&path, 0, None).expect("draining mid-write");
    assert_eq!(drained.lines.len(), 1, "half a record must not be parsed");
    assert_eq!(
        drained.next_offset,
        complete.len() as u64,
        "the offset must stop at the last newline, or the partial line is lost"
    );
}
