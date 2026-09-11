//! The Copilot spool row: is the drain keeping everything it reads?
//!
//! ## Why this row exists at all
//!
//! `copilot push` runs on the schedule [`crate::schedule`] installs, and a wake
//! that fails on every fire looks exactly like a working one from inside VS
//! Code -- Copilot appends to the spool either way. The `copilot drain` row
//! next door reports whether the schedule is *running*; this one reports
//! whether it is *keeping what it reads*. Both needed, so the states below
//! are chosen around that failure, not around tidiness:
//!
//! | State | Meaning |
//! |---|---|
//! | checkpoint unreadable (red) | `copilot-push.json` will not parse |
//! | not enabled (yellow) | no spool file yet: Copilot has not exported |
//! | `<n>` record(s) discarded (red/yellow) | data was consumed and never delivered |
//! | held, waiting for a later record (yellow) | see below |
//! | up to date (green) | nothing pending, nothing lost; the count is the offset, reclaim resets it to 0 |
//! | pending (yellow) | bytes waiting, and a push has succeeded before |
//! | never pushed (red) | bytes waiting and no push has *ever* succeeded |
//! | unknown (yellow) | the state directory could not be resolved |
//!
//! One more, layered on top of any of the above except an already-red one
//! (see [`with_size_warning`]): past [`SIZE_WARNING_ABOVE`] on disk, the row
//! escalates to red regardless of state. #230/#241 (164 MB) and two later
//! incidents (600+ GiB, ~1 TiB) all looked exactly like "up to date" or an
//! ordinary "pending" right up until someone happened to `ls` the state
//! directory -- this is the row noticing first. `copilot::spool::reclaim`'s
//! own module doc explains why the file cannot simply be rewritten to avoid
//! this the way `otel_daemon::spool::compact` does for its own,
//! differently-shaped spool (Copilot holds long-lived descriptors on this
//! one); making the growth visible early is the safety net in place of a
//! silent fix.
//!
//! ## Why "held" is its own row, not a backlog
//!
//! A record the collector refuses on its own is only given up on once it has
//! been shown to accept *something*. When the refused record is the **last**
//! one in the spool there is nothing after it to prove that with, so it is
//! held -- unlike every other stall, no later wake resolves it; it clears
//! only when Copilot appends another record. An ordinary "N bytes pending
//! ... run `copilot push`" would be actively misleading here: that command
//! reproduces the same wake and exits 1 again for the same bytes.
//!
//! ## Why discards outrank "pending", and are not permanently red
//!
//! A parser regression is the failure this row is worst at showing without
//! them: every record classifies as unrecognised, both payloads come out
//! empty, no POST is made, and the checkpoint advances over the lot -- bytes
//! pending then reads 0, so the row would say "up to date", in green, while
//! the entire spool went in the bin. Discards beat `pending` and green, and
//! fade to yellow after a day: cumulative and permanently red is a row people
//! stop reading. Recent loss is the alarm; old loss is a note, never green.

use std::time::{SystemTime, UNIX_EPOCH};

use super::style::{Colour, since};
use crate::{config::OauthConfig, copilot::SpoolStatus, profile::Profile};

/// How recent a discard has to be to still be an alarm -- long enough to
/// survive a night, short enough that a lost record last spring stays quiet.
const FRESH_DISCARD_SECONDS: u64 = 24 * 60 * 60;

/// `None`, not an error, on a clock before the epoch -- `status` reports.
fn now_unix() -> Option<u64> {
    Some(SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs())
}

pub struct Spool {
    /// `pub(super)` so `dashboard`'s tests can render every state from a
    /// literal instead of planting files under a fake `$HOME`; every real
    /// caller goes through [`Self::survey`].
    pub(super) inner: Option<SpoolStatus>,
    /// Seconds since the last successful push, resolved at survey time so
    /// [`Self::row`] stays a pure function of already-collected data.
    pub(super) last_push_age: Option<u64>,
    /// Seconds since the last discarded record, for the same reason.
    pub(super) last_discard_age: Option<u64>,
    /// Seconds the drain has been held on the spool's final record, likewise.
    pub(super) held_age: Option<u64>,
    /// Under `daemon`, Copilot writes no spool at all (#272) -- mirrors
    /// `Drain`'s/`Daemon`'s own `profile` field: without it, a healthy
    /// `daemon` install reads permanent yellow (#302 review).
    pub(super) profile: Profile,
}

impl Spool {
    pub fn survey(config: &OauthConfig) -> Self {
        let inner = SpoolStatus::survey(config);
        let age_of = |at: Option<u64>| Some(now_unix()?.saturating_sub(at?));
        let last_push_age = inner.as_ref().and_then(|s| age_of(s.last_push_unix));
        let last_discard_age = inner.as_ref().and_then(|s| age_of(s.last_discard_unix));
        let held_age = inner.as_ref().and_then(|s| age_of(s.held_since_unix));
        Self {
            inner,
            last_push_age,
            last_discard_age,
            held_age,
            profile: config.profile,
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
        with_size_warning(self.base_row(status), status)
    }

    /// Everything this row reports before [`with_size_warning`] wraps it --
    /// unchanged from before that existed, just split out so the warning
    /// applies uniformly to every branch below rather than being repeated in
    /// each one.
    fn base_row(&self, status: &SpoolStatus) -> (String, Colour, String) {
        if status.checkpoint_unreadable {
            return (
                "checkpoint unreadable".to_owned(),
                Colour::Red,
                format!(
                    "{} will not parse: run `governance-auth copilot push` to see why",
                    status.path.display()
                ),
            );
        }

        if !status.present() {
            if self.profile == Profile::Daemon {
                // Not a gap: `daemon` never creates this file (#272).
                return (
                    "not applicable".to_owned(),
                    Colour::None,
                    "daemon profile: Copilot exports directly, no spool used".to_owned(),
                );
            }
            // Not "you forgot to configure it" -- `configure` writes the file
            // exporter itself; a missing spool just means no export yet.
            return (
                "not enabled".to_owned(),
                Colour::Yellow,
                format!(
                    "no spool at {}: run `governance-auth configure`, then restart VS Code and \
                     send one chat turn",
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

        // Before `pending` -- see the module doc's "held" section.
        if status.held_since_unix.is_some() {
            return (
                "held, waiting for a later record".to_owned(),
                Colour::Yellow,
                format!(
                    "the collector refuses the last record in the spool and there is nothing \
                     after it to prove the collector still works with, so it is held rather than \
                     discarded{}. Re-running `copilot push` repeats this exactly; it clears when \
                     Copilot writes another record. {last}",
                    match self.held_age {
                        Some(age) => format!(" (since {})", since(age)),
                        None => String::new(),
                    }
                ),
            );
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
            format!("{last}; run `governance-auth copilot push`"),
        )
    }

    fn discarded_row(&self, status: &SpoolStatus, last: &str) -> (String, Colour, String) {
        let recent = self
            .last_discard_age
            .is_none_or(|a| a < FRESH_DISCARD_SECONDS);
        let colour = if recent { Colour::Red } else { Colour::Yellow };
        let when = match self.last_discard_age {
            Some(age) => format!("last {}", since(age)),
            None => "at an unknown time".to_owned(),
        };
        (
            format!("{} record(s) discarded", status.discarded_total),
            colour,
            format!(
                "consumed but never delivered, {when}; {last}. Run `governance-auth copilot push \
                 --dry-run` to see what this build cannot read"
            ),
        )
    }
}

/// Above this, the row escalates regardless of which state it would
/// otherwise be in -- detection, not a fix. `copilot::spool::reclaim`'s own
/// module doc explains why a rewrite-based reclaim (the fix
/// `otel_daemon::spool::compact` uses for its own, differently-shaped spool)
/// is NOT safe to port onto this one: Copilot holds long-lived `O_APPEND`
/// descriptors on this file, so rewriting it risks corrupting an in-flight
/// write rather than merely losing one. Absent that option, the honest
/// remaining lever is making the growth visible long before it is a crisis.
///
/// 50 MiB: comfortably above the worst *ordinary* backlog this row's own
/// module doc measures (single-digit MB before a wake catches up), and
/// comfortably below where the last two incidents were noticed (164 MB on
/// the machine that reported #230/#241; 600+ GiB and ~1 TiB on two others
/// that had no warning like this one at all).
pub(super) const SIZE_WARNING_ABOVE: u64 = 50 * 1024 * 1024;

/// Wraps whatever [`Spool::base_row`] returned: past [`SIZE_WARNING_ABOVE`],
/// escalates to red with an explanatory note, UNLESS the row is already red
/// for a more specific reason (an unreadable checkpoint, a drain that has
/// never once pushed, recent discards) -- that row already has the reader's
/// attention, and duplicating the alarm on top of it would just be noise.
fn with_size_warning(
    base: (String, Colour, String),
    status: &SpoolStatus,
) -> (String, Colour, String) {
    let (value, colour, note) = base;
    let Some(size) = status.size else {
        return (value, colour, note);
    };
    if size <= SIZE_WARNING_ABOVE || colour == Colour::Red {
        return (value, colour, note);
    }
    (
        value,
        Colour::Red,
        format!(
            "{note}. WARNING: this spool is {size} bytes on disk, past the \
             {SIZE_WARNING_ABOVE}-byte warning threshold -- see #230/#241 and confirm the drain's \
             schedule is actually running (the `copilot drain` row below)"
        ),
    )
}
