//! Tests for the in-memory spool, including the #290 review's P1-3/P2-4
//! fixes. Split out of `mod.rs` purely for the LoC ceiling.

use super::*;

#[test]
fn retain_and_drain_is_fifo() {
    let mut spool = Spool::new();
    spool.retain(Signal::Logs, b"one".to_vec()).expect("retain");
    spool
        .retain(Signal::Metrics, b"two".to_vec())
        .expect("retain");
    spool
        .retain(Signal::Logs, b"three".to_vec())
        .expect("retain");
    assert_eq!(spool.pending_count(), 3);
    assert_eq!(
        spool.drain_one(),
        Some((Signal::Logs, b"one".to_vec())),
        "the signal must be retained with its payload"
    );
    assert_eq!(spool.drain_one(), Some((Signal::Metrics, b"two".to_vec())));
    assert_eq!(spool.drain_one(), Some((Signal::Logs, b"three".to_vec())));
    assert_eq!(spool.drain_one(), None);
    assert_eq!(spool.pending(), 0);
}

/// #290 review, P2-4: a record put back after a failed attempt must
/// return to the FRONT, not jump to the back of a still-pending backlog.
#[test]
fn requeue_front_restores_fifo_order_after_a_failed_attempt() {
    let mut spool = Spool::new();
    spool.retain(Signal::Logs, b"one".to_vec()).expect("retain");
    spool
        .retain(Signal::Metrics, b"two".to_vec())
        .expect("retain");

    // "one" is drained for an attempt that then fails and puts it back.
    let (signal, payload) = spool.drain_one().expect("one pending");
    spool.requeue_front(signal, payload);

    assert_eq!(
        spool.drain_one(),
        Some((Signal::Logs, b"one".to_vec())),
        "the requeued record must be offered again before the next one, not after it"
    );
    assert_eq!(spool.drain_one(), Some((Signal::Metrics, b"two".to_vec())));
}

#[test]
fn requeue_front_is_never_refused_by_capacity() {
    let mut spool = Spool::new();
    spool
        .retain(Signal::Logs, vec![0u8; CAPACITY])
        .expect("fill exactly");
    let (signal, payload) = spool.drain_one().expect("pending");
    // The payload this spool already held once must not be refused just
    // because `pending()` briefly reads as full.
    spool.requeue_front(signal, payload);
    assert_eq!(spool.pending(), CAPACITY);
    assert_eq!(spool.pending_count(), 1);
}

#[test]
fn pending_tracks_total_bytes() {
    let mut spool = Spool::new();
    spool.retain(Signal::Logs, vec![0u8; 5]).expect("retain");
    spool.retain(Signal::Metrics, vec![0u8; 7]).expect("retain");
    assert_eq!(spool.pending(), 12);
    spool.drain_one();
    assert_eq!(spool.pending(), 7);
}

#[test]
fn at_capacity_retain_refuses_rather_than_dropping() {
    let mut spool = Spool::new();
    spool
        .retain(Signal::Logs, vec![0u8; CAPACITY])
        .expect("fill exactly");
    // One more byte must refuse, not evict the oldest.
    let error = spool
        .retain(Signal::Metrics, vec![0u8; 1])
        .expect_err("a payload over capacity must refuse");
    assert!(
        format!("{error:#}").contains("spool full"),
        "names the condition: {error:#}"
    );
    assert_eq!(spool.pending_count(), 1, "nothing may be evicted");
}
