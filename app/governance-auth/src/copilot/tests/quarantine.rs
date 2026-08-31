//! The evidence table behind "give up on this record".
//!
//! These are the properties the integration tests in
//! `tests/copilot_push_flaky.rs` exercise end to end; asserting them here as
//! well is what makes a change to the threshold or the pruning fail loudly
//! rather than showing up as one integration test going a different colour.

use super::super::quarantine::{Quarantine, REFUSALS_BEFORE_DISCARD};

const NOW: u64 = 1_788_191_916;
const A_WEEK: u64 = 7 * 24 * 60 * 60;

fn key(text: &str) -> String {
    Quarantine::key(text)
}

/// THE rule. One wake's 400 can come from a proxy; a record must survive it.
#[test]
fn one_refusal_is_never_enough() {
    let mut quarantine = Quarantine::default();
    assert!(
        !quarantine.refused(&key("a record"), NOW),
        "a record refused once must be held, not given up on -- a gateway answering 400 for its \
         own reasons is indistinguishable from a bad payload on a single wake"
    );
}

#[test]
fn the_threshold_is_reached_by_repeated_refusals_of_the_same_record() {
    let mut quarantine = Quarantine::default();
    let key = key("a record");
    for round in 1..REFUSALS_BEFORE_DISCARD {
        assert!(!quarantine.refused(&key, NOW), "round {round}");
    }
    assert!(quarantine.refused(&key, NOW), "the last round must decide");
}

/// Two different records are two different pieces of evidence. Without a
/// content-derived key, refusing three *different* records would look like
/// three refusals of one.
#[test]
fn refusals_of_different_records_do_not_accumulate_against_each_other() {
    let mut quarantine = Quarantine::default();
    for index in 0..10 {
        assert!(
            !quarantine.refused(&key(&format!("record {index}")), NOW),
            "record {index} has only ever been refused once"
        );
    }
}

/// A record refused once and then accepted leaves an entry nothing will ever
/// clear. Without expiry, a flake a year ago would still count towards a
/// discard today.
#[test]
fn an_entry_nobody_refused_again_expires_rather_than_counting_for_ever() {
    let mut quarantine = Quarantine::default();
    let key = key("a record");
    assert!(!quarantine.refused(&key, NOW));

    quarantine.prune(NOW + A_WEEK + 1);

    assert!(
        !quarantine.refused(&key, NOW + A_WEEK + 1),
        "the stale refusal must have been forgotten, so this counts as the first one again"
    );
}

/// A record that has been given up on is never offered again, so its entry is
/// dead weight -- and the table lives in a file this process rewrites on every
/// wake.
#[test]
fn a_discarded_record_leaves_no_entry_behind() {
    let mut quarantine = Quarantine::default();
    let key = key("a record");
    for _ in 0..REFUSALS_BEFORE_DISCARD {
        quarantine.refused(&key, NOW);
    }
    quarantine.forget(&key);
    assert!(
        !quarantine.refused(&key, NOW),
        "with the entry gone this is a first refusal again"
    );
}

/// The key must not be the record. `AGENTS.md` bans writing a payload
/// anywhere, and this one is prompt-adjacent telemetry.
#[test]
fn the_key_is_a_digest_and_carries_none_of_the_record() {
    let body = "copilot_chat.tool.call: manage_todo_list";
    let digest = Quarantine::key(body);
    assert_eq!(digest.len(), 32, "128 bits of a SHA-256, hex encoded");
    assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(
        !digest.contains("copilot") && !digest.contains("todo"),
        "the digest must not carry the record: {digest}"
    );
    assert_eq!(digest, Quarantine::key(body), "and it must be stable");
    assert_ne!(digest, Quarantine::key("something else"));
}
