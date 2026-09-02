//! The Copilot spool row's states.
//!
//! Two of them are why this row exists at all:
//!
//! - `never_pushed_with_bytes_waiting` -- a timer that was never enabled or
//!   that fails on every wake, indistinguishable from a healthy install
//!   anywhere else a developer looks.
//! - `discarded_records_are_never_green` -- a parser regression that consumes
//!   the whole spool and delivers none of it. Nothing is pending afterwards,
//!   because the checkpoint kept pace with the loss, so every other signal in
//!   this row reads exactly like "up to date".

use std::path::PathBuf;

use super::*;
use crate::{copilot::SpoolStatus, dashboard::style::Colour};

/// `pub(super)` so `spool_held` can build the same shape: a field added to
/// `SpoolStatus` must break one fixture, not silently miss a second copy of it.
pub(super) fn spool(
    size: Option<u64>,
    offset: u64,
    last_push_unix: Option<u64>,
    age: Option<u64>,
) -> Spool {
    Spool {
        inner: Some(SpoolStatus {
            path: PathBuf::from("/state/governance-auth/copilot-otel.jsonl"),
            size,
            offset,
            pending: size.unwrap_or_default().saturating_sub(offset),
            last_push_unix,
            held_since_unix: None,
            discarded_total: 0,
            last_discard_unix: None,
            checkpoint_unreadable: false,
        }),
        last_push_age: age,
        last_discard_age: None,
        held_age: None,
    }
}

/// A spool that looks perfectly drained -- and lost `discarded` records doing
/// it, `age` seconds ago.
fn discarding(discarded: u64, age: Option<u64>) -> Spool {
    let mut row = spool(Some(4096), 4096, None, None);
    if let Some(status) = row.inner.as_mut() {
        status.discarded_total = discarded;
        status.last_discard_unix = Some(1_788_191_916);
    }
    row.last_discard_age = age;
    row
}

#[test]
fn no_spool_file_reads_as_not_enabled_with_the_way_to_get_one() {
    // The advice changed with the cutover and the test has to change with it:
    // `configure` writes `exporterType`/`outfile` now, so telling a developer
    // to paste them by hand sends them to edit a file this binary owns. What
    // is still on them is restarting VS Code and sending a turn.
    let (value, colour, note) = spool(None, 0, None, None).row();
    assert_eq!(value, "not enabled");
    assert_eq!(colour, Colour::Yellow, "an unused feature is not a fault");
    assert!(
        note.contains("governance-auth configure") && note.contains("restart VS Code"),
        "the note must say how to get a spool, got: {note}"
    );
}

#[test]
fn nothing_pending_reads_as_up_to_date() {
    let (value, colour, note) = spool(Some(4096), 4096, Some(1_788_191_916), Some(120)).row();
    assert!(value.contains("up to date"), "{value}");
    assert_eq!(colour, Colour::Green);
    assert!(note.contains("last push"), "{note}");
    assert!(
        note.contains("2m ago"),
        "elapsed time, not a raw epoch: {note}"
    );
}

/// THE row this whole module exists for. Bytes are waiting and no push has
/// ever succeeded -- the observable signature of a timer that never ran.
#[test]
fn never_pushed_with_bytes_waiting_is_red() {
    let (value, colour, note) = spool(Some(9000), 0, None, None).row();
    assert_eq!(value, "9000 bytes pending");
    assert_eq!(
        colour,
        Colour::Red,
        "a drain that has never once succeeded must not look like an ordinary backlog"
    );
    assert!(note.contains("never pushed"), "{note}");
    assert!(note.contains("copilot-push"), "{note}");
}

/// Pending but previously successful is the ordinary state between timer
/// wakes. Colouring it red would train the reader to ignore the row, which is
/// how the case above stays invisible.
#[test]
fn pending_after_a_previous_push_is_yellow() {
    let (value, colour, _) = spool(Some(9000), 4096, Some(1_788_191_916), Some(45)).row();
    assert_eq!(value, "4904 bytes pending");
    assert_eq!(colour, Colour::Yellow);
}

/// THE regression guard for the silent-loss case. The spool is fully drained
/// and nothing is pending -- because the drain consumed three records it could
/// not read and moved the checkpoint past them. Every other input to this row
/// says "up to date, green"; only the discard counter knows better.
#[test]
fn discarded_records_are_never_green() {
    let (value, colour, note) = discarding(3, Some(60)).row();
    assert_eq!(value, "3 record(s) discarded");
    assert_ne!(
        colour,
        Colour::Green,
        "the checkpoint kept pace with the loss, so `pending == 0` here means nothing"
    );
    assert_eq!(colour, Colour::Red, "a loss an hour ago is an alarm");
    assert!(
        note.contains("never delivered"),
        "the note must say what happened, got: {note}"
    );
}

/// The counter is cumulative and there is no command to reset it, so a red
/// that never clears would train the reader to ignore this row -- which is the
/// same failure the row exists to prevent, one level up. Old loss stays
/// visible; it stops shouting.
#[test]
fn a_discard_older_than_a_day_is_yellow_not_red() {
    let (_, colour, _) = discarding(1, Some(3 * 24 * 60 * 60)).row();
    assert_eq!(colour, Colour::Yellow);
    assert_ne!(colour, Colour::Green, "but still not green");
}

/// The documented table has to contain every row this can actually produce:
/// an unresolvable state directory was rendering a value absent from it.
#[test]
fn an_unresolvable_state_directory_reads_as_unknown() {
    let nothing = Spool {
        inner: None,
        last_push_age: None,
        last_discard_age: None,
        held_age: None,
    };
    let (value, colour, note) = nothing.row();
    assert_eq!(value, "unknown");
    assert_eq!(colour, Colour::Yellow);
    assert!(note.contains("state directory"), "{note}");
}

#[test]
fn an_unreadable_checkpoint_is_reported_rather_than_hidden() {
    let mut broken = spool(Some(9000), 0, None, None);
    if let Some(status) = broken.inner.as_mut() {
        status.checkpoint_unreadable = true;
    }
    let (value, colour, note) = broken.row();
    assert_eq!(colour, Colour::Red);
    assert_eq!(
        value, "checkpoint unreadable",
        "the documented table lists this as the row's VALUE; it was rendering as a note beside \
         the spool's path, which is not what the docs promise"
    );
    assert!(note.contains("will not parse"), "{note}");
}

#[test]
fn the_row_appears_in_the_rendered_table() {
    let out = render(
        "https://auth.example",
        "cli",
        &session(true, true),
        &otel(None, false),
        &spool(Some(9000), 0, None, None),
        &unsurveyed_drain(),
        &[],
    );
    assert!(out.contains("copilot spool"), "{out}");
    assert!(out.contains("9000 bytes pending"), "{out}");
}
