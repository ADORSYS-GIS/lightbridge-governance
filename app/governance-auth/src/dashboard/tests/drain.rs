//! The Copilot drain row's states.
//!
//! The one that earns the row is `a_schedule_that_is_not_installed_is_red`:
//! `configure` turns Copilot's file exporter on, so from that moment the spool
//! grows whether or not anything drains it, and from inside VS Code the two
//! cases look identical. Every other signal a developer has says "configured".
//!
//! `an_unaskable_scheduler_is_not_reported_as_stopped` is the other one worth
//! keeping: `systemctl --user is-active` exits non-zero both for a stopped
//! timer and for a machine with no user manager to ask, so an implementation
//! that read the exit code alone would send half the users of a container to
//! debug a timer that does not exist.

use std::path::PathBuf;

use super::*;
use crate::{dashboard::style::Colour, schedule::Schedule};

fn drain(installed: bool, active: Option<bool>, collector: bool) -> Drain {
    Drain {
        schedule: Some(Schedule {
            path: PathBuf::from(
                "/home/dev/.config/systemd/user/governance-auth-copilot-push.timer",
            ),
            installed,
            active,
        }),
        collector,
    }
}

#[test]
fn a_running_schedule_is_green_and_says_how_often() {
    let (value, colour, note) = drain(true, Some(true), true).row();
    assert_eq!(value, "every 300s");
    assert_eq!(colour, Colour::Green);
    assert!(note.is_empty(), "a healthy row must not add noise: {note}");
}

#[test]
fn a_schedule_that_is_not_installed_is_red() {
    let (value, colour, note) = drain(false, None, true).row();
    assert_eq!(value, "not scheduled");
    assert_eq!(
        colour,
        Colour::Red,
        "the file exporter is already on, so this means the spool grows and nothing ships it"
    );
    assert!(note.contains("governance-auth configure"), "{note}");
}

#[test]
fn an_installed_but_stopped_schedule_names_the_command_that_starts_it() {
    let (value, colour, note) = drain(true, Some(false), true).row();
    assert_eq!(value, "installed, not running");
    assert_eq!(colour, Colour::Red);
    assert!(
        note.contains("launchctl") || note.contains("systemctl"),
        "the note must name a command that works on this platform: {note}"
    );
}

#[test]
fn an_unaskable_scheduler_is_not_reported_as_stopped() {
    let (value, colour, note) = drain(true, None, true).row();
    assert_eq!(value, "installed");
    assert_eq!(
        colour,
        Colour::Yellow,
        "unknown is not the same as stopped, and must not be coloured like it"
    );
    assert!(note.contains("could not ask"), "{note}");
}

#[test]
fn no_collector_means_this_row_is_information_not_an_alarm() {
    // The telemetry row above already says "not configured". A second red row
    // for the same fact trains the reader to skip both.
    let (value, colour, _) = drain(false, None, false).row();
    assert_eq!(value, "not scheduled");
    assert_eq!(colour, Colour::None);
}

#[test]
fn the_row_appears_in_the_rendered_table() {
    let out = render(
        "https://auth.example",
        "cli",
        &session(true, true),
        &otel(None, false),
        &spool::spool(Some(0), 0, None, None),
        &drain(true, Some(true), true),
        &[],
    );
    assert!(out.contains("copilot drain"), "{out}");
    assert!(out.contains("every 300s"), "{out}");
}
