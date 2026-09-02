//! The daemon service row -- #271, ADR-0016.
//!
//! ⚠️ **This is the failure mode the daemon introduces, and it is worse than
//! what it replaces.** Today a broken drain leaves bytes on disk and turns
//! the Copilot spool row red. A dead daemon can lose telemetry with
//! **nothing on disk to recover**: a client fire-and-forgetting OTLP at a
//! closed loopback port gets no error anyone reads, and from inside the
//! editor it looks identical to working. Reporting it is a requirement, not
//! a nicety.
//!
//! Structurally this is [`super::drain::Drain`] again -- same three-valued
//! survey (`schedule::daemon::survey`, which reuses `systemd::classify`),
//! same "installed but not running is red and names the fix" shape. What is
//! new here is the profile gate: this row is only an alarm under `daemon`.
//! Under `manual` the service is deliberately absent, and reporting that as
//! a problem would be the exact false alarm ADR-0016 itself calls out
//! elsewhere ("no OTLP token: Codex cannot export" on a machine with no
//! Codex installed).

use std::path::Path;

use super::style::Colour;
use crate::{
    config::OauthConfig,
    profile::Profile,
    schedule::{self, Schedule},
};

pub struct Daemon {
    pub(super) schedule: Option<Schedule>,
    pub(super) profile: Profile,
}

impl Daemon {
    pub fn survey(home: Option<&Path>, config: &OauthConfig) -> Self {
        Self {
            schedule: home.map(schedule::daemon::survey),
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

        if self.profile != Profile::Daemon {
            // A LEFTOVER service is worth naming rather than papering over --
            // same reasoning as `Drain::row`'s own leftover branch. #270 AC5
            // removes it on a real profile switch; surfacing it here is the
            // backstop for a removal that failed (no user session to ask,
            // for instance).
            if schedule.installed {
                return (
                    "installed, manual profile".to_owned(),
                    Colour::Yellow,
                    format!(
                        "{} is still installed under the `manual` profile: run `governance-auth \
                         configure --profile manual` again, or remove it by hand",
                        schedule.path.display()
                    ),
                );
            }
            return (
                "not applicable".to_owned(),
                Colour::None,
                "manual profile: telemetry is exported directly, not via the daemon".to_owned(),
            );
        }

        if !schedule.installed {
            return (
                "not installed".to_owned(),
                Colour::Red,
                "nothing forwards telemetry: run `governance-auth configure` to install the \
                 daemon"
                    .to_owned(),
            );
        }

        match schedule.active {
            Some(true) => ("running".to_owned(), Colour::Green, String::new()),
            Some(false) => (
                "installed, not running".to_owned(),
                Colour::Red,
                format!("start it with `{}`", schedule::daemon::start_command()),
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
