//! `DurableSpool`'s quarantine/probe-discard mechanics (#269/#291 review,
//! P1-3). Split from [`super`] purely for the LoC gate.

use super::{Signal, TempDir};

#[test]
fn a_record_refused_once_is_retried_not_discarded() {
    let dir = TempDir::new("quarantine-once");
    let mut spool = dir.spool("a");
    spool
        .retain(Signal::Logs, b"maybe".to_vec())
        .expect("retain");

    let pending = spool.next().expect("next").expect("pending");
    let eligible = spool.record_refusal(&pending).expect("record the refusal");
    assert!(!eligible, "one refusal is never enough on its own");

    let still_pending = spool
        .next()
        .expect("next")
        .expect("the same record must still be offered");
    assert_eq!(still_pending.payload, b"maybe");
}

/// #269/#291 review, P1-3: two separate refusals alone must NOT discard --
/// [`crate::otel_daemon::spool::DurableSpool::record_refusal`] only says a
/// record is *eligible*. Discarding it still needs
/// [`crate::otel_daemon::spool::DurableSpool::peek_next`] to find, and prove
/// accepted, a later record -- exercised by
/// [`a_record_refused_twice_with_a_confirmed_probe_is_discarded`] below. This
/// test is the other half: `peek_next` returning nothing (no later record
/// exists yet) must leave the stuck record exactly where it was, not discard
/// it on the strength of the refusal count alone -- the bug the review
/// found, reproduced here and left failing until the fix landed.
#[test]
fn a_record_refused_twice_with_nothing_after_it_stays_held() {
    let dir = TempDir::new("quarantine-twice-exhausted");
    let mut spool = dir.spool("a");
    spool
        .retain(Signal::Logs, b"never".to_vec())
        .expect("retain");

    let first = spool.next().expect("next").expect("pending");
    assert!(!spool.record_refusal(&first).expect("refusal 1"));
    let second = spool.next().expect("next").expect("still pending");
    assert!(
        spool.record_refusal(&second).expect("refusal 2"),
        "a second separate refusal makes it eligible"
    );

    assert!(
        spool.peek_next(&second).expect("peek").is_none(),
        "fixture: nothing exists past the stuck record yet"
    );
    // Eligibility alone changes nothing on disk: the record is still there,
    // still the next thing `next` offers.
    let still_pending = spool
        .next()
        .expect("next")
        .expect("eligible is not the same as discarded");
    assert_eq!(still_pending.payload, b"never");
}

/// The confirming half of P1-3: once a probe past the stuck record is itself
/// proven -- offered on its own, decoded, and (in `drain::advance_one`,
/// exercised at a higher level in `tests/serve_otel_durability.rs`) accepted
/// by the collector -- `discard_confirmed` commits through it in one write,
/// discarding the stuck record and delivering the probe together.
#[test]
fn a_record_refused_twice_with_a_confirmed_probe_is_discarded() {
    let dir = TempDir::new("quarantine-twice-confirmed");
    let mut spool = dir.spool("a");
    spool
        .retain(Signal::Logs, b"stuck".to_vec())
        .expect("retain stuck");
    spool
        .retain(Signal::Logs, b"probe".to_vec())
        .expect("retain probe");

    let stuck = spool.next().expect("next").expect("stuck pending");
    assert!(!spool.record_refusal(&stuck).expect("refusal 1"));
    // `next` still returns the same (still-pending) stuck record on a second
    // read, since nothing has advanced past it yet.
    let stuck_again = spool.next().expect("next").expect("still stuck");
    assert!(
        spool.record_refusal(&stuck_again).expect("refusal 2"),
        "eligible now"
    );

    let probe = spool
        .peek_next(&stuck_again)
        .expect("peek")
        .expect("the second retained record is available to probe with");
    assert_eq!(probe.payload, b"probe");

    spool
        .discard_confirmed(&stuck_again, &probe)
        .expect("discard, delivering the probe in the same commit");

    assert!(
        spool.next().expect("next").is_none(),
        "both records are now resolved: the stuck one discarded, the probe delivered"
    );
}

/// [`crate::otel_daemon::spool::DurableSpool::peek_next`] must never itself
/// act on what it finds -- a probe that silently discarded on the caller's
/// behalf would make looking ahead as destructive as consuming, defeating
/// the whole point of having a read-only check.
#[test]
fn peek_next_does_not_advance_the_checkpoint() {
    let dir = TempDir::new("peek-is-read-only");
    let mut spool = dir.spool("a");
    spool.retain(Signal::Logs, b"one".to_vec()).expect("retain");
    spool.retain(Signal::Logs, b"two".to_vec()).expect("retain");

    let first = spool.next().expect("next").expect("first pending");
    let peeked = spool
        .peek_next(&first)
        .expect("peek")
        .expect("second record visible to a peek");
    assert_eq!(peeked.payload, b"two");

    // The peek must not have moved anything: `next` still returns the FIRST
    // record, unchanged.
    let still_first = spool.next().expect("next").expect("first still pending");
    assert_eq!(still_first.payload, b"one");
}
