//! The otel daemon's own spool row's states.
//!
//! `held` is the one this row exists for -- see `otel_spool`'s own module
//! doc for the incident (this exact daemon, held on a refused record with
//! nothing in `status` saying so) it makes visible.

use std::path::PathBuf;

use super::*;
use crate::{dashboard::style::Colour, otel_daemon::DaemonSpoolStatus};

/// `pub(super)` for the same reason `spool::spool` is: a field added to
/// `DaemonSpoolStatus` should break one fixture, not silently miss a copy.
pub(super) fn otel_spool(
    size: Option<u64>,
    offset: u64,
    discarded_total: u64,
    last_discard_unix: Option<u64>,
    held: Option<(usize, u32, u64)>,
) -> OtelSpool {
    OtelSpool {
        inner: Some(DaemonSpoolStatus {
            path: PathBuf::from("/state/governance-auth/otel-daemon-spool.jsonl"),
            size,
            offset,
            pending: size.unwrap_or_default().saturating_sub(offset),
            discarded_total,
            last_discard_unix,
            checkpoint_unreadable: false,
            held,
        }),
        worst_quarantined_age: held.map(|_| 0),
        last_discard_age: last_discard_unix.map(|_| 0),
        profile: crate::profile::Profile::Daemon,
    }
}

#[test]
fn manual_profile_is_not_applicable_regardless_of_what_was_surveyed() {
    let mut row = otel_spool(Some(9000), 0, 0, None, None);
    row.profile = crate::profile::Profile::Manual;
    let (value, colour, _) = row.row();
    assert_eq!(value, "not applicable");
    assert_eq!(colour, Colour::None);
}

#[test]
fn an_unresolvable_state_directory_reads_as_unknown() {
    let row = OtelSpool {
        inner: None,
        worst_quarantined_age: None,
        last_discard_age: None,
        profile: crate::profile::Profile::Daemon,
    };
    let (value, colour, _) = row.row();
    assert_eq!(value, "unknown");
    assert_eq!(colour, Colour::Yellow);
}

#[test]
fn an_unreadable_checkpoint_is_reported_rather_than_hidden() {
    let mut row = otel_spool(Some(4096), 0, 0, None, None);
    if let Some(status) = row.inner.as_mut() {
        status.checkpoint_unreadable = true;
    }
    let (value, colour, note) = row.row();
    assert_eq!(value, "checkpoint unreadable");
    assert_eq!(colour, Colour::Red);
    assert!(note.contains("otel-daemon-wedged.md"), "{note}");
}

#[test]
fn no_spool_file_yet_is_informational_not_a_warning() {
    let (value, colour, _) = otel_spool(None, 0, 0, None, None).row();
    assert_eq!(value, "no data yet");
    assert_eq!(colour, Colour::None);
}

#[test]
fn nothing_pending_reads_as_up_to_date() {
    let (value, colour, _) = otel_spool(Some(4096), 4096, 0, None, None).row();
    assert_eq!(value, "up to date (4096 bytes)");
    assert_eq!(colour, Colour::Green);
}

#[test]
fn bytes_pending_with_no_quarantine_is_an_ordinary_backlog() {
    let (value, colour, note) = otel_spool(Some(9000), 4096, 0, None, None).row();
    assert_eq!(value, "4904 bytes pending");
    assert_eq!(colour, Colour::Yellow);
    assert!(note.contains("retries continuously"), "{note}");
}

/// The state this row exists for: pending bytes AND a record currently held
/// behind repeated refusals. Must name the runbook, and must not claim a
/// verdict ("wedged") this command cannot reach alone -- see the module
/// doc's "What this row cannot tell you, and says so".
#[test]
fn a_held_record_is_reported_distinctly_from_ordinary_pending() {
    let (value, colour, note) =
        otel_spool(Some(9000), 4096, 0, None, Some((3, 5, 1_788_191_916))).row();
    assert_eq!(value, "3 record(s) held, worst refused 5 time(s)");
    assert_eq!(colour, Colour::Yellow);
    assert!(note.contains("self-heals"), "{note}");
    assert!(note.contains("otel-daemon-wedged.md"), "{note}");
    assert!(
        !note.contains("wedged forever") && !note.contains("permanently"),
        "must not claim a verdict this command cannot reach from one snapshot: {note}"
    );
}

#[test]
fn a_recent_discard_is_red() {
    let (value, colour, note) = otel_spool(Some(4096), 4096, 2, Some(0), None).row();
    assert_eq!(value, "2 record(s) discarded");
    assert_eq!(colour, Colour::Red);
    assert!(note.contains("otel-daemon-wedged.md"), "{note}");
}

#[test]
fn an_old_discard_fades_to_yellow() {
    let mut row = otel_spool(Some(4096), 4096, 488, Some(1_788_191_916), None);
    row.last_discard_age = Some(48 * 60 * 60);
    let (_, colour, _) = row.row();
    assert_eq!(
        colour,
        Colour::Yellow,
        "a day-old-plus discard is history, not a live alarm"
    );
}

/// Discarded outranks held: a discard is proven, permanent loss, and must
/// not be shadowed by a currently-held record from a different, unrelated
/// episode.
#[test]
fn discarded_outranks_held() {
    let (value, ..) = otel_spool(Some(9000), 4096, 1, Some(0), Some((1, 2, 0))).row();
    assert_eq!(value, "1 record(s) discarded");
}

#[test]
fn the_row_appears_in_the_rendered_table() {
    let out = table(
        "https://auth.example",
        "cli",
        &session(true, true),
        &otel(None, false),
        &[],
    );
    assert!(out.contains("otel spool"), "{out}");
}
