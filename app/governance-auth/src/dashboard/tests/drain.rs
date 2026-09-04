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
use crate::{dashboard::style::Colour, profile::Profile, schedule::Schedule};

/// `manual`, matching every test below written before the profile split --
/// see [`daemon_profile_drain`] for the `daemon`-profile fixtures.
fn drain(installed: bool, active: Option<bool>, collector: bool) -> Drain {
    stale_drain(installed, active, collector, Some(false), Profile::Manual)
}

fn daemon_profile_drain(installed: bool, active: Option<bool>) -> Drain {
    stale_drain(installed, active, true, Some(false), Profile::Daemon)
}

fn stale_drain(
    installed: bool,
    active: Option<bool>,
    collector: bool,
    stale: Option<bool>,
    profile: Profile,
) -> Drain {
    Drain {
        schedule: Some(Schedule {
            path: PathBuf::from(
                "/home/dev/.config/systemd/user/governance-auth-copilot-push.timer",
            ),
            installed,
            active,
        }),
        collector,
        stale,
        profile,
    }
}

/// The upgrade case: a timer that is installed AND running, but running a
/// command this binary no longer has. Green on `active` alone would be the
/// most confident wrong line on the table -- it wakes every five minutes to
/// fail on a clap parse error nobody reads. Pins that staleness is checked
/// before `active`, not after.
#[test]
fn a_schedule_written_by_an_older_version_is_red_and_names_configure() {
    let (value, colour, note) =
        stale_drain(true, Some(true), true, Some(true), Profile::Manual).row();
    assert_eq!(value, "out of date", "{note}");
    assert_eq!(colour, Colour::Red);
    assert!(note.contains("governance-auth configure"), "{note}");
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
        &Surveys {
            telemetry: &otel(None, false),
            daemon: &unsurveyed_daemon(),
            spool: &spool::spool(Some(0), 0, None, None),
            drain: &drain(true, Some(true), true),
        },
        &[],
    );
    assert!(out.contains("copilot drain"), "{out}");
    assert!(out.contains("every 300s"), "{out}");
}

#[test]
fn a_leftover_schedule_with_no_collector_is_not_reported_as_unscheduled() {
    // The row used to answer `!collector` first and in no colour, so a timer
    // that `configure` failed to remove read as "nothing to see here" while it
    // woke every five minutes and failed. Both facts are true at once; the
    // one worth printing is the one a developer can act on.
    let (value, colour, note) = drain(true, Some(true), false).row();
    assert_eq!(value, "scheduled, no collector");
    assert_eq!(colour, Colour::Yellow);
    assert!(
        note.contains("still installed") && note.contains("--otel-endpoint"),
        "must name the leftover and how to resolve it, got: {note}"
    );
}

#[test]
fn no_collector_and_no_schedule_is_plain_information() {
    // The other half of the pair: with nothing installed there is nothing to
    // act on, and colouring this would train the reader to ignore the row.
    let (value, colour, note) = drain(false, None, false).row();
    assert_eq!(value, "not scheduled");
    assert_eq!(colour, Colour::None);
    assert_eq!(note, "no collector configured");
}

/// Found running #270+#271 together against a real machine, not by any unit
/// test: every fixture above predates the profile split, so `collector:
/// true, schedule.installed: false` only ever meant "`configure` failed" to
/// them. #270 AC5 made it also mean "working as designed, under `daemon`" --
/// this pins that the row tells the two apart, and -- as important -- that
/// the note stops telling the reader to run `configure`, which does nothing
/// under `daemon` (the timer is deliberately never installed there).
#[test]
fn daemon_profile_with_no_schedule_is_yellow_not_red_and_names_the_real_fix() {
    let (value, colour, note) = daemon_profile_drain(false, None).row();
    assert_eq!(value, "not scheduled");
    assert_eq!(
        colour,
        Colour::Yellow,
        "not a `configure` failure -- #270 AC5 removes this timer under `daemon` on purpose"
    );
    assert!(
        !note.contains("governance-auth configure"),
        "must not suggest a fix that does nothing under `daemon`: {note}"
    );
    assert!(
        note.contains("manual"),
        "must name the fix that actually works: {note}"
    );
}

/// The mirror of the leftover case above, under `daemon`: #270 AC5's
/// retraction should have removed this, so still finding it installed is
/// the backstop for a removal that failed, not silence.
#[test]
fn a_leftover_drain_under_daemon_profile_is_yellow_not_silent() {
    let (value, colour, note) = daemon_profile_drain(true, Some(true)).row();
    assert_eq!(value, "scheduled, daemon profile");
    assert_eq!(colour, Colour::Yellow);
    assert!(
        note.contains("daemon") && note.contains("configure"),
        "must name the leftover and how to resolve it, got: {note}"
    );
}
