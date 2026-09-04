//! `DurableSpool`, exercised end to end. Quarantine/probe-discard tests
//! live in [`quarantine`]; reclaim and `is_empty` tests live in [`reclaim`]
//! -- both split out for the LoC gate.

mod quarantine;
mod reclaim;

use std::path::PathBuf;

use super::DurableSpool;
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

/// #269/#291 review, P2-5: a payload at [`super::MAX_RETAINABLE_PAYLOAD`]
/// must actually be retainable on an otherwise-empty spool -- falsifies the
/// bug the finding described (a body sized to the OLD ceiling, `CAPACITY`
/// itself, encoded larger than `CAPACITY` and could never be retained at
/// all).
#[test]
fn the_largest_retainable_payload_is_actually_retainable() {
    let dir = TempDir::new("max-payload");
    let mut spool = dir.spool("a");
    let payload = vec![0u8; super::MAX_RETAINABLE_PAYLOAD];
    spool
        .retain(Signal::Logs, payload.clone())
        .expect("the documented ceiling must fit on an empty spool");
    let pending = spool.next().expect("next").expect("pending");
    assert_eq!(pending.payload, payload);
}

/// The other half of P2-5: the encoded line for [`super::MAX_RETAINABLE_PAYLOAD`]
/// must stay under `copilot::spool::MAX_READ`, the tighter of the two
/// ceilings -- otherwise `next` would find no terminating newline and bail
/// rather than ever returning the record.
#[test]
fn the_largest_retainable_payload_stays_under_the_tail_readers_own_cap() {
    let dir = TempDir::new("max-payload-read");
    let mut spool = dir.spool("a");
    spool
        .retain(Signal::Logs, vec![0u8; super::MAX_RETAINABLE_PAYLOAD])
        .expect("retain");
    spool
        .next()
        .expect("the tail reader must find this record's newline, not bail")
        .expect("pending");
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
