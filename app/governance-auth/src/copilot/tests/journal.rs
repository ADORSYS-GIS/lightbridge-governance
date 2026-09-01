//! What a commit costs, and what it refuses to cost.
//!
//! The granularity argument in [`super::super::journal`] is only sound if a
//! wake that resolves nothing writes nothing and an ordinary wake writes once
//! per signal. Both are asserted here rather than left as prose, because the
//! cheap way to make a kill-safe drain is to write on every record and the
//! only thing stopping that is a number nobody measures.

use std::path::PathBuf;

use super::{
    super::{checkpoint::Checkpoint, journal::Journal, push::Signal, spool},
    log_line,
};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "copilot-journal-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("creating the scratch directory");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Three log lines in a scratch spool, drained so the offsets are the real
/// ones rather than invented.
fn drained(dir: &TempDir) -> spool::Drain {
    let path = dir.0.join("spool.jsonl");
    let contents: String = (0..3).map(|_| format!("{}\n", log_line())).collect();
    std::fs::write(&path, contents).expect("writing the spool fixture");
    spool::drain(&path, 0, None).expect("draining")
}

/// The fail-closed half. "There is no checkpoint" is how a drain that has
/// never delivered anything is meant to look, and a journal that created the
/// file just by existing would take that signal away from `status`.
#[test]
fn a_wake_that_resolved_nothing_does_not_even_create_the_file() {
    let dir = TempDir::new("nothing");
    let drain = drained(&dir);
    let path = dir.0.join("copilot-push.json");
    let mut journal = Journal::new(
        path.clone(),
        &drain.lines,
        Checkpoint::default(),
        drain.identity,
        false,
    );

    journal.commit().expect("committing nothing");

    assert_eq!(journal.writes(), 0);
    assert!(
        !path.exists(),
        "a wake that delivered nothing must leave `status` reading 'never pushed', not a \
         checkpoint at byte 0"
    );
}

/// The ordinary wake: one accepted batch per signal, one write per signal. If
/// this ever climbs with the record count, the granularity policy has silently
/// become "write per record" and a large drain is IO-bound.
#[test]
fn resolving_a_whole_range_costs_one_write_per_signal() {
    let dir = TempDir::new("ordinary");
    let drain = drained(&dir);
    let mut journal = Journal::new(
        dir.0.join("copilot-push.json"),
        &drain.lines,
        Checkpoint::default(),
        drain.identity,
        false,
    );

    journal
        .advance(Signal::Metrics, drain.next_offset, 0)
        .expect("metrics");
    journal
        .advance(Signal::Logs, drain.next_offset, 0)
        .expect("logs");

    assert_eq!(journal.writes(), 2, "one per signal, not one per record");
    assert_eq!(journal.state().offset, drain.next_offset);
}

/// Committing the same position again is free. The DFS commits defensively at
/// the top of every loop turn, so a non-advancing commit has to be a no-op or
/// a bisect would write once per *range* rather than once per advance.
#[test]
fn committing_the_same_position_twice_writes_once() {
    let dir = TempDir::new("idempotent");
    let drain = drained(&dir);
    let mut journal = Journal::new(
        dir.0.join("copilot-push.json"),
        &drain.lines,
        Checkpoint::default(),
        drain.identity,
        false,
    );

    for _ in 0..5 {
        journal
            .advance(Signal::Metrics, drain.next_offset, 0)
            .expect("metrics");
    }
    journal.commit().expect("no-op commit");

    assert_eq!(journal.writes(), 1);
}

/// Conservation, at the level the journal is responsible for: the write that
/// moves the offset is the same write that records what it moved past.
#[test]
fn the_write_that_advances_the_offset_also_records_the_loss() {
    let dir = TempDir::new("conserve");
    let path = dir.0.join("spool.jsonl");
    // Two records this build cannot read at all, so nothing is deliverable and
    // the only honest outcome is a counted loss.
    std::fs::write(&path, "not json at all\nnor is this\n").expect("writing the spool fixture");
    let drain = spool::drain(&path, 0, None).expect("draining");
    let checkpoint_path = dir.0.join("copilot-push.json");
    let mut journal = Journal::new(
        checkpoint_path.clone(),
        &drain.lines,
        Checkpoint::default(),
        drain.identity,
        false,
    );

    journal
        .advance(Signal::Metrics, drain.next_offset, 0)
        .expect("metrics");
    journal
        .advance(Signal::Logs, drain.next_offset, 0)
        .expect("logs");

    let stored = super::super::checkpoint::load(&checkpoint_path).expect("loading");
    assert_eq!(stored.offset, drain.next_offset);
    assert_eq!(
        stored.discarded_total, 2,
        "the offset moved past two records; a durable offset beside a loss count that is not \
         durable is the conservation rule breaking at every kill"
    );
}
