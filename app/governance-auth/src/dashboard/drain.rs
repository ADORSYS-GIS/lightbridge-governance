//! The Copilot drain row: is anything actually shipping the spool?
//!
//! Sits directly under the spool row and answers the other half of the same
//! question. The spool row says how many bytes are waiting; this one says
//! whether a scheduler is going to come and get them. Before `configure`
//! installed the timer itself the honest answer was "we have no idea" -- the
//! runbook told the developer to install a systemd unit or a launchd agent and
//! nothing here could see whether they had. Now the schedule is ours, so
//! reporting it is ours too.
//!
//! ## Three-valued on purpose
//!
//! `active` is `Option<bool>`: a scheduler that could not be asked is
//! `None`, never `false`. Claiming a drain is stopped when the question was
//! never answered sends a developer to debug a timer that is running fine, and
//! is the same class of error as claiming it runs when it does not.

use std::path::Path;

use super::style::Colour;
use crate::{
    config::OauthConfig,
    profile::Profile,
    schedule::{self, INTERVAL_SECONDS, Schedule},
};

pub struct Drain {
    pub(super) schedule: Option<Schedule>,
    /// Whether a collector is configured at all. With none, there is nothing
    /// to schedule and this row is information, not an alarm -- the telemetry
    /// row above already carries that.
    pub(super) collector: bool,
    /// Whether the unit/plist on disk still matches what `configure` would
    /// write. `None` when it could not be asked -- see
    /// [`crate::schedule::stale`], which is also where the reason this is
    /// checked at all lives.
    pub(super) stale: Option<bool>,
    /// #270 AC5 made this timer `manual`-only: `configure` under `daemon`
    /// removes it even with a collector configured, so `!schedule.installed`
    /// stopped meaning "configure failed" the moment that shipped. Without
    /// this field the row could not tell the two apart and called the
    /// intentional case a bug, in red, with a fix ("run configure") that
    /// does nothing under `daemon` -- found by running this against a real
    /// `daemon`-profile machine, not by any unit test, because every
    /// existing fixture predates the profile split.
    pub(super) profile: Profile,
}

impl Drain {
    pub fn survey(home: Option<&Path>, config: &OauthConfig) -> Self {
        Self {
            schedule: home.map(schedule::survey),
            collector: config.otel_endpoint.is_some(),
            stale: home.and_then(|home| schedule::stale(home, config)),
            profile: config.profile,
        }
    }

    pub(super) fn row(&self) -> (String, Colour, String) {
        let Some(schedule) = &self.schedule else {
            return (
                "unknown".to_owned(),
                Colour::Yellow,
                "could not locate the home directory".to_owned(),
            );
        };

        if !self.collector {
            // A LEFTOVER schedule is worth naming rather than papering over.
            // `configure` removes the timer when the collector goes away, so
            // one still on disk means that removal did not happen -- and it
            // wakes every INTERVAL_SECONDS to fail, for ever, while a bare
            // "not scheduled" in no colour reads as nothing to see here.
            if schedule.installed {
                return (
                    "scheduled, no collector".to_owned(),
                    Colour::Yellow,
                    format!(
                        "{} is still installed and fails on every wake: re-run `governance-auth \
                         configure --otel-endpoint <url>`, or remove it by hand",
                        schedule.path.display()
                    ),
                );
            }
            return (
                "not scheduled".to_owned(),
                Colour::None,
                "no collector configured".to_owned(),
            );
        }

        // #270 AC5: `configure` under `daemon` removes this timer on
        // purpose, collector or not -- everything below this point assumes
        // `manual`, where a missing schedule means `configure` failed.
        if self.profile == Profile::Daemon {
            if schedule.installed {
                // Leftover, same idiom as `dashboard::Daemon::row`'s own
                // manual-profile leftover branch: #270 AC5's retraction is
                // supposed to remove this, so still finding it is the
                // backstop for a removal that failed, not silence.
                return (
                    "scheduled, daemon profile".to_owned(),
                    Colour::Yellow,
                    format!(
                        "{} is still installed under the `daemon` profile: run `governance-auth \
                         configure` again, or remove it by hand",
                        schedule.path.display()
                    ),
                );
            }
            // Not red: this is #270's own intended state, not a broken
            // install. Not silent either -- #272 has not yet rewired
            // Copilot to point its own OTLP exporter at the daemon, so the
            // spool genuinely goes undrained under `daemon` today, and the
            // fix that actually works is named instead of the generic
            // `configure` re-run that does nothing here.
            return (
                "not scheduled".to_owned(),
                Colour::Yellow,
                "daemon profile: Copilot's spool is not yet drained (pending #272) -- switch to \
                 `--profile manual` if you need Copilot telemetry now"
                    .to_owned(),
            );
        }

        if !schedule.installed {
            // Red, and worded as the consequence. Copilot's file exporter is
            // on by now, so "not scheduled" means the spool grows and nothing
            // ever ships it -- which from inside VS Code looks like a working
            // install.
            return (
                "not scheduled".to_owned(),
                Colour::Red,
                format!(
                    "nothing drains the spool: run `governance-auth configure` to install {}",
                    schedule.path.display()
                ),
            );
        }

        // Before `active`: a running timer that invokes a command this
        // binary no longer has is WORSE than a stopped one -- it wakes every
        // INTERVAL_SECONDS, fails on a parse error nobody reads, and reports
        // itself as green on the line below. This is the row an upgrade makes
        // wrong, so it is the row that has to say so.
        if self.stale == Some(true) {
            return (
                "out of date".to_owned(),
                Colour::Red,
                format!(
                    "{} does not match what this version writes: run `governance-auth configure`",
                    schedule.path.display()
                ),
            );
        }

        match schedule.active {
            Some(true) => (
                format!("every {INTERVAL_SECONDS}s"),
                Colour::Green,
                String::new(),
            ),
            Some(false) => (
                "installed, not running".to_owned(),
                Colour::Red,
                format!("start it with `{}`", schedule::start_command()),
            ),
            None => (
                "installed".to_owned(),
                Colour::Yellow,
                format!(
                    "could not ask the scheduler whether {} is running",
                    schedule.path.display()
                ),
            ),
        }
    }
}
