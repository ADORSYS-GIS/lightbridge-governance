//! The Copilot spool row's four states.
//!
//! The one this row exists for is `never_pushed_with_bytes_waiting`: that is a
//! timer that was never enabled or that fails on every wake, and it is
//! indistinguishable from a healthy install anywhere else a developer looks.

use std::path::PathBuf;

use super::*;
use crate::{copilot::SpoolStatus, dashboard::style::Colour};

fn spool(size: Option<u64>, offset: u64, last_push_unix: Option<u64>, age: Option<u64>) -> Spool {
    Spool {
        inner: Some(SpoolStatus {
            path: PathBuf::from("/state/governance-auth/copilot-otel.jsonl"),
            size,
            offset,
            pending: size.unwrap_or_default().saturating_sub(offset),
            last_push_unix,
            checkpoint_unreadable: false,
        }),
        last_push_age: age,
    }
}

#[test]
fn no_spool_file_reads_as_not_enabled_with_the_setting_to_paste() {
    let (value, colour, note) = spool(None, 0, None, None).row();
    assert_eq!(value, "not enabled");
    assert_eq!(colour, Colour::Yellow, "an unused feature is not a fault");
    assert!(
        note.contains("exporterType") && note.contains("outfile"),
        "the note must name what to set, got: {note}"
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

#[test]
fn an_unreadable_checkpoint_is_reported_rather_than_hidden() {
    let mut broken = spool(Some(9000), 0, None, None);
    if let Some(status) = broken.inner.as_mut() {
        status.checkpoint_unreadable = true;
    }
    let (_, colour, note) = broken.row();
    assert_eq!(colour, Colour::Red);
    assert!(note.contains("checkpoint unreadable"), "{note}");
}

#[test]
fn the_row_appears_in_the_rendered_table() {
    let out = render(
        "https://auth.example",
        "cli",
        &session(true, true),
        &otel(None, false),
        &spool(Some(9000), 0, None, None),
        &[],
    );
    assert!(out.contains("copilot spool"), "{out}");
    assert!(out.contains("9000 bytes pending"), "{out}");
}
