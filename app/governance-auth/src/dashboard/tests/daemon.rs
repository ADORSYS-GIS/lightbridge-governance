//! The daemon row's states -- #271.
//!
//! `an_unaskable_daemon_is_not_reported_as_stopped` is THE test AC6's
//! falsification names: reduce `Schedule::active` handling to a boolean (so
//! `None` collapses into the `Some(false)` arm) and this fails, because
//! `stopped_and_unknown_render_differently` pins that the two render as
//! different values with different colours, not merely that each looks
//! right in isolation.

use std::path::PathBuf;

use super::*;
use crate::{dashboard::style::Colour, profile::Profile, schedule::Schedule};

fn daemon(installed: bool, active: Option<bool>, profile: Profile) -> Daemon {
    Daemon {
        schedule: Some(Schedule {
            path: PathBuf::from(
                "/home/dev/.config/systemd/user/governance-auth-serve-otel.service",
            ),
            installed,
            active,
        }),
        profile,
    }
}

#[test]
fn a_running_daemon_is_green() {
    let (value, colour, note) = daemon(true, Some(true), Profile::Daemon).row();
    assert_eq!(value, "running");
    assert_eq!(colour, Colour::Green);
    assert!(note.is_empty(), "a healthy row must not add noise: {note}");
}

/// #271 AC2: installed but not running is red, and the note names the fix.
#[test]
fn an_installed_but_stopped_daemon_names_the_command_that_starts_it() {
    let (value, colour, note) = daemon(true, Some(false), Profile::Daemon).row();
    assert_eq!(value, "installed, not running");
    assert_eq!(colour, Colour::Red);
    assert!(
        note.contains("launchctl") || note.contains("systemctl"),
        "the note must name a command that works on this platform: {note}"
    );
}

#[test]
fn a_daemon_that_is_not_installed_under_the_daemon_profile_is_red() {
    let (value, colour, note) = daemon(false, None, Profile::Daemon).row();
    assert_eq!(value, "not installed");
    assert_eq!(
        colour,
        Colour::Red,
        "nothing forwards telemetry under `daemon` with no service installed"
    );
    assert!(note.contains("governance-auth configure"), "{note}");
}

/// #271 AC1/AC6: `None` (could not ask) must never collapse into
/// `Some(false)` (confirmed stopped) -- different value, different colour.
/// A boolean-typed classification cannot represent this distinction at all,
/// which is exactly the falsification AC6 asks for.
#[test]
fn stopped_and_unknown_render_differently() {
    let stopped = daemon(true, Some(false), Profile::Daemon).row();
    let unknown = daemon(true, None, Profile::Daemon).row();
    assert_ne!(
        stopped.0, unknown.0,
        "value must differ: {stopped:?} vs {unknown:?}"
    );
    assert_ne!(
        stopped.1, unknown.1,
        "colour must differ: {stopped:?} vs {unknown:?}"
    );
}

#[test]
fn an_unaskable_daemon_is_not_reported_as_stopped() {
    let (value, colour, note) = daemon(true, None, Profile::Daemon).row();
    assert_eq!(value, "installed");
    assert_eq!(
        colour,
        Colour::Yellow,
        "unknown is not the same as stopped, and must not be coloured like it"
    );
    assert!(note.contains("could not ask"), "{note}");
}

/// #271 AC3: under `manual` this row is information, not an alarm -- the
/// service is DELIBERATELY absent, unlike the `daemon`-profile case above.
#[test]
fn manual_profile_with_no_daemon_installed_is_plain_information() {
    let (value, colour, note) = daemon(false, None, Profile::Manual).row();
    assert_eq!(value, "not applicable");
    assert_eq!(colour, Colour::None);
    assert!(note.contains("manual profile"), "{note}");
}

/// The other half of #270 AC5's own backstop: a service `configure` failed
/// to remove on switching to `manual` is worth naming, not silently ignored
/// the way "not applicable" would read.
#[test]
fn a_leftover_daemon_under_manual_profile_is_yellow_not_silent() {
    let (value, colour, note) = daemon(true, Some(true), Profile::Manual).row();
    assert_eq!(value, "installed, manual profile");
    assert_eq!(colour, Colour::Yellow);
    assert!(
        note.contains("manual") && note.contains("configure"),
        "must name the leftover and how to resolve it, got: {note}"
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
            daemon: &daemon(true, Some(true), Profile::Daemon),
            spool: &spool::spool(Some(0), 0, None, None),
            drain: &unsurveyed_drain(),
        },
        &[],
    );
    assert!(out.contains("daemon"), "{out}");
    assert!(out.contains("running"), "{out}");
}
