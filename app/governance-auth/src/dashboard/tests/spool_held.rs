//! The held row: a stall that reads as an ordinary backlog everywhere else.
//!
//! Split from [`super::spool`] to keep both under the 200-LoC gate, and because
//! this row answers a different question from the rest of them. Every other
//! state in that table is "how much is waiting"; this one is "waiting will not
//! help". The bytes are pending either way, so the numbers are identical and
//! only the advice differs -- and the advice the backlog row gives ("run
//! `governance-auth copilot push`") reproduces the same failing wake.

use super::{spool::spool, *};
use crate::dashboard::style::Colour;

/// A drain stuck on a refused record that is the last one in the spool.
fn held(age: Option<u64>) -> Spool {
    let mut row = spool(Some(9000), 4096, Some(1_788_191_916), Some(45));
    if let Some(status) = row.inner.as_mut() {
        status.held_since_unix = Some(1_788_191_900);
    }
    row.held_age = age;
    row
}

#[test]
fn a_drain_held_on_the_last_record_does_not_read_as_an_ordinary_backlog() {
    let (value, colour, note) = held(Some(3600)).row();

    assert_eq!(value, "held, waiting for a later record");
    assert_eq!(colour, Colour::Yellow, "held is not lost");
    assert!(
        note.contains("clears when Copilot writes another record"),
        "the note must say what actually resolves it, got: {note}"
    );
    assert!(
        !note.contains("run `governance-auth copilot push`"),
        "advising the command that reproduces the stall is worse than saying nothing: {note}"
    );
    // Whatever `style::since` renders, not a raw epoch -- the same treatment
    // every other age in this table gets.
    assert!(note.contains("60m ago"), "and how long it has been: {note}");
}

/// A row that renders correctly in isolation but never gets chosen is the same
/// as no row at all, and `status` is the only place a developer finds out.
#[test]
fn the_held_row_reaches_the_rendered_table() {
    let out = render(
        "https://auth.example",
        "cli",
        &session(true, true),
        &Surveys {
            telemetry: &otel(None, false),
            daemon: &unsurveyed_daemon(),
            spool: &held(None),
            drain: &unsurveyed_drain(),
        },
        &[],
    );
    assert!(out.contains("held, waiting for a later record"), "{out}");
    assert!(!out.contains("bytes pending"), "{out}");
}

/// Without a readable clock the age is absent, not zero -- and the row must
/// still name the state rather than degrading to the backlog wording.
#[test]
fn an_unknown_hold_age_still_reads_as_held() {
    let (value, _, note) = held(None).row();
    assert_eq!(value, "held, waiting for a later record");
    assert!(!note.contains("(since"), "{note}");
}
