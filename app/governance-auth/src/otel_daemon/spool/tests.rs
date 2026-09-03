//! `DurableSpool`, exercised end to end against a scratch directory.

use std::path::PathBuf;

use super::{DurableSpool, commit::RECLAIM_ABOVE};
use crate::copilot::Signal;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "otel-daemon-spool-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("creating the scratch directory");
        Self(path)
    }

    fn spool(&self, spool_ext: &str) -> DurableSpool {
        DurableSpool::at(
            self.0.join(format!("spool.{spool_ext}")),
            self.0.join(format!("checkpoint.{spool_ext}.json")),
        )
        .expect("opening a fresh scratch spool")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_retained_record_is_returned_then_gone_after_advance() {
    let dir = TempDir::new("roundtrip");
    let mut spool = dir.spool("a");

    assert!(spool.next().expect("next").is_none(), "nothing yet");
    spool.retain(Signal::Logs, b"one".to_vec()).expect("retain");

    let pending = spool.next().expect("next").expect("one record pending");
    assert_eq!(pending.signal, Signal::Logs);
    assert_eq!(pending.payload, b"one");

    spool.advance(&pending).expect("advance");
    assert!(
        spool.next().expect("next").is_none(),
        "delivered records must not be re-offered"
    );
}

#[test]
fn two_records_are_delivered_in_fifo_order() {
    let dir = TempDir::new("fifo");
    let mut spool = dir.spool("a");
    spool
        .retain(Signal::Logs, b"first".to_vec())
        .expect("retain");
    spool
        .retain(Signal::Metrics, b"second".to_vec())
        .expect("retain");

    let first = spool.next().expect("next").expect("first pending");
    assert_eq!(first.payload, b"first");
    spool.advance(&first).expect("advance");

    let second = spool.next().expect("next").expect("second pending");
    assert_eq!(second.payload, b"second");
    assert_eq!(second.signal, Signal::Metrics);
    spool.advance(&second).expect("advance");

    assert!(spool.next().expect("next").is_none());
}

#[test]
fn retain_refuses_rather_than_dropping_once_full() {
    let dir = TempDir::new("capacity");
    let mut spool = dir.spool("a");
    // Each record encodes larger than it is (base64 + the JSON envelope), so
    // fill in chunks and stop at the first refusal rather than assuming a
    // raw byte count lines up with the encoded one.
    let chunk = vec![0u8; 1024 * 1024];
    let mut retained = 0;
    let error = loop {
        match spool.retain(Signal::Logs, chunk.clone()) {
            Ok(()) => {
                retained += 1;
                assert!(retained <= 32, "capacity should have refused by now");
            }
            Err(error) => break error,
        }
    };
    assert!(
        format!("{error:#}").contains("spool full"),
        "names the condition: {error:#}"
    );
    assert!(retained > 0, "some room must exist below capacity");
}

#[test]
fn a_corrupt_line_is_skipped_and_counted_not_fatal() {
    let dir = TempDir::new("corrupt");
    let spool_path = dir.0.join("spool.corrupt");
    let checkpoint_path = dir.0.join("checkpoint.corrupt.json");
    let mut spool = DurableSpool::at(spool_path.clone(), checkpoint_path).expect("open");

    // A line that could never have come from `envelope::encode` -- simulating
    // the torn-write case the module doc names.
    std::fs::write(&spool_path, b"not an envelope\n").expect("seed garbage");
    spool
        .retain(Signal::Logs, b"good".to_vec())
        .expect("retain");

    let pending = spool
        .next()
        .expect("next must skip the garbage, not fail")
        .expect("the good record must still be reached");
    assert_eq!(pending.payload, b"good");
}

#[test]
fn a_record_refused_once_is_retried_not_discarded() {
    let dir = TempDir::new("quarantine-once");
    let mut spool = dir.spool("a");
    spool
        .retain(Signal::Logs, b"maybe".to_vec())
        .expect("retain");

    let pending = spool.next().expect("next").expect("pending");
    let discarded = spool
        .quarantine_or_discard(&pending)
        .expect("record the refusal");
    assert!(!discarded, "one refusal is never enough on its own");

    let still_pending = spool
        .next()
        .expect("next")
        .expect("the same record must still be offered");
    assert_eq!(still_pending.payload, b"maybe");
}

#[test]
fn a_record_refused_twice_is_discarded_and_counted() {
    let dir = TempDir::new("quarantine-twice");
    let mut spool = dir.spool("a");
    spool
        .retain(Signal::Logs, b"never".to_vec())
        .expect("retain");

    let first = spool.next().expect("next").expect("pending");
    assert!(!spool.quarantine_or_discard(&first).expect("refusal 1"));
    let second = spool.next().expect("next").expect("still pending");
    assert!(
        spool.quarantine_or_discard(&second).expect("refusal 2"),
        "a second separate refusal must discard"
    );

    assert!(
        spool.next().expect("next").is_none(),
        "a discarded record must not be offered again"
    );
}

#[test]
fn a_fully_delivered_spool_over_the_reclaim_threshold_is_truncated() {
    let dir = TempDir::new("reclaim");
    let spool_path = dir.0.join("spool.reclaim");
    let checkpoint_path = dir.0.join("checkpoint.reclaim.json");
    let mut spool = DurableSpool::at(spool_path.clone(), checkpoint_path).expect("open");

    // One record safely over RECLAIM_ABOVE, so the very first advance already
    // meets the reclaim precondition (size == offset).
    let big = vec![b'x'; usize::try_from(RECLAIM_ABOVE).unwrap_or(usize::MAX) + 1024];
    spool.retain(Signal::Logs, big).expect("retain");
    let pending = spool.next().expect("next").expect("pending");
    spool.advance(&pending).expect("advance");

    let size = std::fs::metadata(&spool_path).expect("stat").len();
    assert_eq!(
        size, 0,
        "a fully-delivered spool over the threshold must be reclaimed"
    );
}
