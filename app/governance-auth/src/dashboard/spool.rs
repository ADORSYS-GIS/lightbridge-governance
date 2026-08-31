//! The Copilot spool row: is the drain actually running, and is it keeping
//! everything it reads?
//!
//! ## Why this row exists at all
//!
//! `copilot-push` is driven by a systemd user timer or a launchd agent that
//! this binary deliberately does not install. Anything it does not install it
//! also cannot monitor -- and a timer that was never enabled, or that fails on
//! every wake, looks exactly like a working one from inside VS Code. The row
//! below is the only place a developer finds out.
//!
//! So the states are chosen around that failure, not around tidiness:
//!
//! | State | Meaning |
//! |---|---|
//! | checkpoint unreadable (red) | `copilot-push.json` will not parse |
//! | not enabled (yellow) | no spool file: Copilot's file exporter is off |
//! | `<n>` record(s) discarded (red/yellow) | data was consumed and never delivered |
//! | up to date (green)   | spool exists, nothing pending, nothing lost |
//! | pending (yellow)     | bytes waiting, and a push has succeeded before |
//! | never pushed (red)   | bytes waiting and no push has *ever* succeeded |
//! | unknown (yellow)     | the state directory could not be resolved |
//!
//! ## Why discards outrank "pending", and why they are not permanently red
//!
//! A parser regression is the failure this row is worst at showing without
//! them: every record classifies as unrecognised, both payloads come out
//! empty, no POST is made, and the checkpoint advances over the lot. Bytes
//! pending then reads 0 -- so the row said "up to date", in green, while the
//! entire spool went in the bin. Discards therefore beat `pending` and beat
//! green.
//!
//! They fade to yellow after a day, because the counter is cumulative and a
//! row that is red forever with no way to clear it is a row people stop
//! reading -- which is the same failure again, one level up. Recent loss is
//! the alarm; old loss is a note. Neither is green.

use std::time::{SystemTime, UNIX_EPOCH};

use super::style::{Colour, since};
use crate::{config::OauthConfig, copilot::SpoolStatus};

/// How recent a discard has to be to still be an alarm. One day: long enough
/// to survive a night and a weekend morning, short enough that a single lost
/// record last spring is not still shouting.
const FRESH_DISCARD_SECONDS: u64 = 24 * 60 * 60;

/// `None` rather than an error on a clock before the epoch: `status` reports,
/// it does not assert, and "last push at an unknown time" is still useful.
fn now_unix() -> Option<u64> {
    Some(SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs())
}

pub struct Spool {
    /// `pub(super)` so `dashboard`'s tests can render every state from a
    /// literal instead of planting files under a fake `$HOME`. Nothing outside
    /// this module constructs one; every caller goes through [`Self::survey`].
    pub(super) inner: Option<SpoolStatus>,
    /// Seconds since the last successful push, resolved at survey time so
    /// [`Self::row`] stays a pure function of already-collected data -- the
    /// same reason [`super::style::short`] takes `home` instead of reading it.
    pub(super) last_push_age: Option<u64>,
    /// Seconds since the last discarded record, for the same reason.
    pub(super) last_discard_age: Option<u64>,
}

impl Spool {
    pub fn survey(config: &OauthConfig) -> Self {
        let inner = SpoolStatus::survey(config);
        let age_of = |at: Option<u64>| Some(now_unix()?.saturating_sub(at?));
        let last_push_age = inner.as_ref().and_then(|s| age_of(s.last_push_unix));
        let last_discard_age = inner.as_ref().and_then(|s| age_of(s.last_discard_unix));
        Self {
            inner,
            last_push_age,
            last_discard_age,
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
                "checkpoint unreadable".to_owned(),
                Colour::Red,
                format!(
                    "{} will not parse: run `governance-auth copilot-push` to see why",
                    status.path.display()
                ),
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
            (true, Some(age)) => format!("last push {}", since(age)),
            (true, None) => "last push at an unknown time".to_owned(),
            (false, _) => "never pushed".to_owned(),
        };

        if status.discarded_total > 0 {
            return self.discarded_row(status, &last);
        }

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

    fn discarded_row(&self, status: &SpoolStatus, last: &str) -> (String, Colour, String) {
        let recent = self
            .last_discard_age
            .is_none_or(|age| age < FRESH_DISCARD_SECONDS);
        let colour = if recent { Colour::Red } else { Colour::Yellow };
        let when = match self.last_discard_age {
            Some(age) => format!("last {}", since(age)),
            None => "at an unknown time".to_owned(),
        };
        (
            format!("{} record(s) discarded", status.discarded_total),
            colour,
            format!(
                "consumed but never delivered, {when}; {last}. Run `governance-auth copilot-push \
                 --dry-run` to see what this build cannot read"
            ),
        )
    }
}
