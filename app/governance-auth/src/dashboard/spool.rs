//! The Copilot spool row: is the drain actually running?
//!
//! ## Why this row exists at all
//!
//! `copilot-push` is driven by a systemd user timer or a launchd agent that
//! this binary deliberately does not install. Anything it does not install it
//! also cannot monitor -- and a timer that was never enabled, or that fails on
//! every wake, looks exactly like a working one from inside VS Code. The row
//! below is the only place a developer finds out.
//!
//! So the four states are chosen around that failure, not around tidiness:
//!
//! | State | Meaning |
//! |---|---|
//! | not enabled (yellow) | no spool file: Copilot's file exporter is off |
//! | up to date (green)   | spool exists, nothing pending |
//! | pending (yellow)     | bytes waiting, and a push has succeeded before |
//! | never pushed (red)   | bytes waiting and no push has *ever* succeeded |
//!
//! Red is reserved for the last one on purpose. "Pending" is the ordinary
//! state between timer wakes and colouring it red would train the reader to
//! ignore this line, which is precisely how the stopped timer stays invisible.

use std::time::{SystemTime, UNIX_EPOCH};

use super::style::{Colour, ago};
use crate::{config::OauthConfig, copilot::SpoolStatus};

/// `None` rather than an error on a clock before the epoch: `status` reports,
/// it does not assert, and "last push at an unknown time" is still useful.
fn now_unix() -> Option<u64> {
    Some(SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs())
}

pub struct Spool {
    /// `pub(super)` so `dashboard`'s tests can render all four states from a
    /// literal instead of planting files under a fake `$HOME`. Nothing outside
    /// this module constructs one; every caller goes through [`Self::survey`].
    pub(super) inner: Option<SpoolStatus>,
    /// Seconds since the last successful push, resolved at survey time so
    /// [`Self::row`] stays a pure function of already-collected data -- the
    /// same reason [`super::style::short`] takes `home` instead of reading it.
    pub(super) last_push_age: Option<u64>,
}

impl Spool {
    pub fn survey(config: &OauthConfig) -> Self {
        let inner = SpoolStatus::survey(config);
        let last_push_age = inner
            .as_ref()
            .and_then(|status| status.last_push_unix)
            .and_then(|pushed| Some(now_unix()?.saturating_sub(pushed)));
        Self {
            inner,
            last_push_age,
        }
    }

    /// `(value, colour, note)`, matching the shape every other row uses.
    pub(super) fn row(&self) -> (String, Colour, String) {
        let Some(status) = &self.inner else {
            return (
                "unknown".to_owned(),
                Colour::Yellow,
                "could not locate the state directory".to_owned(),
            );
        };

        if status.checkpoint_unreadable {
            return (
                status.path.display().to_string(),
                Colour::Red,
                "checkpoint unreadable: run `governance-auth copilot-push` to see why".to_owned(),
            );
        }

        if !status.present() {
            return (
                "not enabled".to_owned(),
                Colour::Yellow,
                format!(
                    "set github.copilot.chat.otel.exporterType=\"file\" and outfile={}",
                    status.path.display()
                ),
            );
        }

        let last = match (status.last_push_unix.is_some(), self.last_push_age) {
            // `ago` renders a *remaining* lifetime, so elapsed time is passed
            // as a negative and comes back as "expired <n> ago" -- the same
            // convention the session row's expiry already uses.
            (true, Some(age)) => format!(
                "last push {}",
                ago(i64::try_from(age).unwrap_or(i64::MAX).saturating_neg())
            ),
            (true, None) => "last push at an unknown time".to_owned(),
            (false, _) => "never pushed".to_owned(),
        };

        if status.pending == 0 {
            return (
                format!("up to date ({} bytes)", status.offset),
                Colour::Green,
                last,
            );
        }

        let colour = if status.last_push_unix.is_some() {
            Colour::Yellow
        } else {
            Colour::Red
        };
        (
            format!("{} bytes pending", status.pending),
            colour,
            format!("{last}; run `governance-auth copilot-push`"),
        )
    }
}
