//! The otel daemon's own spool row: is the daemon's outbound leg actually
//! keeping up, or silently stuck behind a run of refused records?
//!
//! ## Why this row exists at all
//!
//! Before this row, `status`'s only signal about the daemon was the `daemon`
//! row next door -- "is the process running". A daemon can be running and
//! still not be forwarding anything: `otel_daemon::drain::lookahead::walk`
//! holds a record it cannot yet prove the collector accepts past, and every
//! wake re-tries the same held record, indistinguishable from a healthy
//! collector taking its time unless someone reads the checkpoint file by
//! hand. That is exactly the incident
//! [`docs/runbooks/otel-daemon-wedged.md`](../../../../docs/runbooks/otel-daemon-wedged.md)
//! documents (this workstation, 2026-09-09: 447 refusals on one record, 384
//! good ones held behind it, nothing in `status` said so).
//!
//! ## What this row cannot tell you, and says so
//!
//! A record currently held looks IDENTICAL, at one point in time, whether it
//! will clear itself on the very next wake (a later record gets accepted,
//! proving the collector, discarding the whole stuck run) or never clears
//! without help (more than `MAX_LOOKAHEAD` consecutive bad records). The
//! runbook's own diagnosis is "check twice, a few minutes apart, and see if
//! `refusals` keeps climbing while `offset` stays put" -- a comparison across
//! two calls to this command, not a fact available inside one. This row
//! reports the same numbers the runbook has a human read by hand (record
//! count, worst refusal count, how long ago it was last refused), in
//! `Colour::Yellow`, honestly short of promising a verdict `status` cannot
//! reach alone.
//!
//! ## Why "discarded" still outranks a held record
//!
//! Same reasoning as `copilot`'s own spool row: a discard is proven,
//! permanent loss, while a held record is a record that has NOT been lost (it
//! is still on disk, still being retried). Reporting "N held" while an
//! earlier, unrelated discard sits unmentioned would bury the more serious of
//! the two.

use super::style::{Colour, since};
use crate::{config::OauthConfig, otel_daemon::DaemonSpoolStatus, profile::Profile};

/// How recent a discard has to be to still be an alarm -- same policy and the
/// same reasoning as `spool::FRESH_DISCARD_SECONDS`: long enough to survive a
/// night, short enough that a loss last spring stays quiet rather than red
/// forever.
const FRESH_DISCARD_SECONDS: u64 = 24 * 60 * 60;

fn now_unix() -> Option<u64> {
    use std::time::{SystemTime, UNIX_EPOCH};
    Some(SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs())
}

pub struct OtelSpool {
    /// `pub(super)` so `dashboard`'s tests can render every state from a
    /// literal, the same reason `Spool::inner` is -- every real caller goes
    /// through [`Self::survey`].
    pub(super) inner: Option<DaemonSpoolStatus>,
    /// Seconds since the worst-off currently-held record was last refused,
    /// resolved at survey time so [`Self::row`] stays a pure function of
    /// already-collected data.
    pub(super) worst_quarantined_age: Option<u64>,
    pub(super) last_discard_age: Option<u64>,
    /// Under `manual`, this daemon does not run at all, so it has no spool --
    /// mirrors `Daemon`'s own `profile` field: without it, a healthy `manual`
    /// install reads as a problem it is not.
    pub(super) profile: Profile,
}

impl OtelSpool {
    pub fn survey(config: &OauthConfig) -> Self {
        let inner = DaemonSpoolStatus::survey();
        let age_of = |at: Option<u64>| Some(now_unix()?.saturating_sub(at?));
        let worst_quarantined_age = inner
            .as_ref()
            .and_then(|status| status.held)
            .and_then(|(_, _, last_seen)| age_of(Some(last_seen)));
        let last_discard_age = inner
            .as_ref()
            .and_then(|status| age_of(status.last_discard_unix));
        Self {
            inner,
            worst_quarantined_age,
            last_discard_age,
            profile: config.profile,
        }
    }

    /// `(value, colour, note)`, matching the shape every other row uses.
    pub(super) fn row(&self) -> (String, Colour, String) {
        if self.profile != Profile::Daemon {
            return (
                "not applicable".to_owned(),
                Colour::None,
                "manual profile: telemetry is exported directly, not through a daemon spool"
                    .to_owned(),
            );
        }

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
                    "{} will not parse -- see docs/runbooks/otel-daemon-wedged.md",
                    status.path.display()
                ),
            );
        }

        if !status.present() {
            return (
                "no data yet".to_owned(),
                Colour::None,
                "the daemon creates this file on first receive; nothing has come through yet"
                    .to_owned(),
            );
        }

        if status.discarded_total > 0 {
            return self.discarded_row(status);
        }

        // Before plain "pending" -- a held record repeats identically on the
        // next wake, so telling the reader to just wait is not obviously
        // wrong the way it would be actively misleading advice.
        if let Some((count, refusals, _)) = status.held {
            let recency = match self.worst_quarantined_age {
                Some(age) => format!(" (most recently {})", since(age)),
                None => String::new(),
            };
            let note = format!(
                "the drain is still trying to prove the collector past a refused record{recency} \
                 -- self-heals once a later one is accepted; if this has not cleared in a few \
                 minutes, see docs/runbooks/otel-daemon-wedged.md"
            );
            return (
                format!("{count} record(s) held, worst refused {refusals} time(s)"),
                Colour::Yellow,
                note,
            );
        }

        if status.pending == 0 {
            return (
                format!("up to date ({} bytes)", status.offset),
                Colour::Green,
                String::new(),
            );
        }

        (
            format!("{} bytes pending", status.pending),
            Colour::Yellow,
            "the collector is unreachable or slow; the daemon retries continuously".to_owned(),
        )
    }

    fn discarded_row(&self, status: &DaemonSpoolStatus) -> (String, Colour, String) {
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
                "consumed but never delivered, {when} -- see docs/runbooks/otel-daemon-wedged.md"
            ),
        )
    }
}
