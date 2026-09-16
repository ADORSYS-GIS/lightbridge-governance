//! The Copilot spool row's states.
//!
//! Two of them are why this row exists at all:
//!
//! - `never_pushed_with_bytes_waiting` -- a timer never enabled or failing on
//!   every wake, indistinguishable from a healthy install anywhere else.
//! - `discarded_records_are_never_green` -- a parser regression that consumes
//!   the whole spool and delivers none of it, with the checkpoint keeping
//!   pace so every other signal reads exactly like "up to date".

use std::path::PathBuf;

use super::*;
use crate::{
    copilot::SpoolStatus,
    dashboard::{spool::SIZE_WARNING_ABOVE, style::Colour},
};

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
        profile: crate::profile::Profile::Manual,
    }
}

/// A perfectly drained spool that lost `discarded` records `age` seconds ago.
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
    // `configure` writes `exporterType`/`outfile` now, so what's still on
    // the developer is restarting VS Code and sending a turn.
    let (value, colour, note) = spool(None, 0, None, None).row();
    assert_eq!(value, "not enabled");
    assert_eq!(colour, Colour::Yellow, "an unused feature is not a fault");
    assert!(
        note.contains("governance-auth configure") && note.contains("restart VS Code"),
        "the note must say how to get a spool, got: {note}"
    );
}

/// #272 / #302 review round 2: under `daemon`, Copilot never writes a spool
/// at all (its exporter posts straight to the loopback daemon), so without
/// a profile gate a healthy install reported permanent yellow "not enabled".
#[test]
fn no_spool_file_under_daemon_profile_is_informational_not_a_warning() {
    let mut row = spool(None, 0, None, None);
    row.profile = crate::profile::Profile::Daemon;
    let (value, colour, note) = row.row();
    assert_eq!(value, "not applicable");
    assert_eq!(
        colour,
        Colour::None,
        "a spool that will never exist under `daemon` is not a gap, so must not be yellow"
    );
    assert!(
        !note.contains("governance-auth configure"),
        "must not suggest a fix for a file `daemon` never creates: {note}"
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

/// THE row this module exists for: bytes waiting, no push ever succeeded --
/// the signature of a timer that never ran.
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
    assert!(note.contains("copilot push"), "{note}");
}

/// Pending but previously successful is the ordinary state between wakes;
/// red here would train the reader to ignore the row entirely.
#[test]
fn pending_after_a_previous_push_is_yellow() {
    let (value, colour, _) = spool(Some(9000), 4096, Some(1_788_191_916), Some(45)).row();
    assert_eq!(value, "4904 bytes pending");
    assert_eq!(colour, Colour::Yellow);
}

/// THE regression guard for the silent-loss case: the drain consumed three
/// unreadable records and moved the checkpoint past them, so every other
/// input says "up to date, green" -- only the discard counter knows better.
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

/// The counter is cumulative with no reset command, so a red that never
/// clears would train the reader to ignore the row. Old loss stays visible,
/// but stops shouting.
#[test]
fn a_discard_older_than_a_day_is_yellow_not_red() {
    let (_, colour, _) = discarding(1, Some(3 * 24 * 60 * 60)).row();
    assert_eq!(colour, Colour::Yellow);
    assert_ne!(colour, Colour::Green, "but still not green");
}

/// The documented table must cover every value this can produce.
#[test]
fn an_unresolvable_state_directory_reads_as_unknown() {
    let nothing = Spool {
        inner: None,
        last_push_age: None,
        last_discard_age: None,
        held_age: None,
        profile: crate::profile::Profile::Manual,
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

/// #230/#241's own shape: pending stays modest, a push keeps succeeding --
/// every other signal here reads healthy while the file itself has grown
/// past where the last two incidents were noticed only once they were
/// already 164 MB / 600+ GiB. The size warning is the one thing left to
/// catch that.
#[test]
fn a_spool_past_the_size_warning_is_red_even_though_it_is_otherwise_up_to_date() {
    let past_threshold = SIZE_WARNING_ABOVE + 1;
    let (value, colour, note) = spool(
        Some(past_threshold),
        past_threshold,
        Some(1_788_191_916),
        Some(120),
    )
    .row();
    assert!(
        value.contains("up to date"),
        "the base state must still be the honest one: {value}"
    );
    assert_eq!(
        colour,
        Colour::Red,
        "otherwise-healthy must not stay green once the file itself is this large"
    );
    assert!(
        note.contains("WARNING") && note.contains(&past_threshold.to_string()),
        "the note must say why it escalated and name the actual size, got: {note}"
    );
}

#[test]
fn a_spool_under_the_size_warning_is_unaffected_by_it() {
    let (_, colour, note) = spool(Some(4096), 4096, Some(1_788_191_916), Some(120)).row();
    assert_eq!(colour, Colour::Green);
    assert!(!note.contains("WARNING"), "{note}");
}

/// A row already red for a more specific reason must not also carry the
/// size warning's own text -- that row already has the reader's attention,
/// and a second, less specific alarm on top of it is noise, not signal.
#[test]
fn a_row_already_red_for_its_own_reason_does_not_also_carry_the_size_warning() {
    let past_threshold = SIZE_WARNING_ABOVE + 1;
    let (_, colour, note) = spool(Some(past_threshold), 0, None, None).row();
    assert_eq!(
        colour,
        Colour::Red,
        "never-pushed-with-bytes-waiting is red on its own"
    );
    assert!(
        !note.contains("WARNING"),
        "the more specific alarm must not be diluted by a second one: {note}"
    );
}

#[test]
fn the_row_appears_in_the_rendered_table() {
    let out = render(
        "https://auth.example",
        "cli",
        &session(true, true),
        &Surveys {
            telemetry: &otel(None, false),
            daemon: &unsurveyed_daemon(),
            otel_spool: &unsurveyed_otel_spool(),
            spool: &spool(Some(9000), 0, None, None),
            drain: &unsurveyed_drain(),
        },
        &[],
    );
    assert!(out.contains("copilot spool"), "{out}");
    assert!(out.contains("9000 bytes pending"), "{out}");
}
