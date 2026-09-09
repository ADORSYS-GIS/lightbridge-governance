//! What happens to a record the collector has refused **on its own**
//! (#269/#291 review, P1-3).
//!
//! Split out of [`super`] to keep both halves under the 200-LoC gate, and
//! because this is the one decision in the drain that destroys data: it is
//! worth reading without the rest of `advance_one` around it.
//!
//! ## Two conditions, and neither is sufficient alone
//!
//! Mirrors `copilot::export::isolate`'s own two conditions for the same
//! reason it needs them:
//!
//! 1. **Refused across separate attempts**
//!    ([`super::spool::DurableSpool::record_refusal`]). A refusal is a
//!    deterministic function of the payload only when nothing sits in front
//!    of the collector; a WAF, a proxy, or an upstream hiccup returns one for
//!    reasons of its own.
//! 2. **The collector has been shown to accept something else.** Otherwise a
//!    collector misconfigured to refuse everything is answered by discarding
//!    the spool one record per attempt -- a five-minute config error turned
//!    into permanent, total data loss.
//!
//! Condition 2 has no free evidence to reuse the way a batched drain's
//! "already delivered something earlier in this pass" does: this daemon
//! offers one record at a time. So it is always proven the same way here --
//! [`super::spool::DurableSpool::peek_next`] finds the next record already
//! waiting behind the stuck one, and it is offered **on its own**, purely to
//! find out whether the collector accepts anything:
//!
//! - **Accepted** -- the collector works. Every record between the stuck one
//!   and this probe, inclusive, is discarded, and the probe's own delivery is
//!   committed in the same write ([`super::spool::DurableSpool::discard_confirmed`]),
//!   so it is never re-offered as if it were still pending.
//! - **Refused** -- not proof by itself, but not necessarily "the collector
//!   is down" either: see "Walking past more than one bad record" below.
//! - **The collector is unreachable** -- not proof. Nothing is discarded,
//!   nothing advances.
//! - **Nothing exists yet past the stuck record** -- there is nothing to
//!   prove the collector with. Held, not discarded; this is the one stall
//!   that does not resolve itself on `pump`'s timer -- it clears once a new
//!   record is retained, and not before.
//!
//! There is deliberately no fallback to "the checkpoint says a forward
//! succeeded a minute ago". That answer is cheap and wrong in exactly the
//! case condition 2 exists for: a collector that worked a minute ago and
//! refuses everything now.
//!
//! ## Walking past more than one bad record
//!
//! `peek_next` only ever looks ONE record ahead per call -- it has to, since
//! deciding whether *that* record is accepted means actually offering it to
//! the collector, and a probe that kept walking indefinitely on its own would
//! turn one refusal into an unbounded burst of credentialed requests. So
//! [`handle`] is the one that walks: if the immediate probe is also refused,
//! it tries the record after THAT, and so on, up to [`MAX_LOOKAHEAD`] records,
//! rather than stopping at the first refusal the way a single `peek_next`
//! call would.
//!
//! This is not a hypothetical widening. Two consecutive corrupted OTLP
//! records (`codex-app-server`, 2026-09-09 -- a genuine protobuf wire-type
//! mismatch, confirmed identical against three different collector versions
//! and the real production endpoint, so not a collector-side bug) wedged a
//! real daemon's drain forever: the one-hop probe found the second corrupted
//! record, was refused, and gave up -- with 384 good records sitting
//! undelivered right behind both. See `docs/runbooks/otel-daemon-wedged.md`.
//! Bounding the walk at [`MAX_LOOKAHEAD`] keeps a genuinely dead collector
//! from turning into "retry the entire remaining spool on every wake" --
//! past that many consecutive refusals, this reads as outage, not a run of
//! bad records, and holds exactly as it always has.

use super::{Outcome, probe::probe_accepted, with_spool};
use crate::otel_daemon::{DaemonState, checkpoint, spool::Pending};

/// How many records ahead of a stuck one [`handle`] is willing to try before
/// concluding the collector itself is the problem, not a short run of bad
/// records. See the module doc's "Walking past more than one bad record".
const MAX_LOOKAHEAD: usize = 20;

/// Handles a `Verdict::Refused` outcome for `pending`: records the refusal,
/// and -- only once it is both eligible AND confirmed, per the module doc --
/// discards it.
pub(super) async fn handle(
    state: &DaemonState,
    pending: Pending,
    status: axum::http::StatusCode,
) -> Outcome {
    let now = match checkpoint::now_unix() {
        Ok(now) => now,
        Err(error) => {
            tracing::error!(error = %error, "could not read the system clock to record a refusal");
            return Outcome::Stopped;
        }
    };
    let eligible = match with_spool(state, {
        let pending = pending.clone();
        move |spool| spool.record_refusal(&pending, now)
    })
    .await
    {
        Ok(eligible) => eligible,
        Err(error) => {
            tracing::error!(error = %error, "could not record a refusal");
            return Outcome::Stopped;
        }
    };
    if !eligible {
        tracing::warn!(
            %status,
            "the collector refused this record; giving it one more separate attempt before \
             discarding it"
        );
        return Outcome::Stopped;
    }

    // Walk forward from `pending` looking for the first LATER record the
    // collector actually accepts -- see the module doc's "Walking past more
    // than one bad record". `cursor` is the record most recently tried;
    // `lost` counts every record given up on so far, `pending` included, so
    // a run of N consecutive bad records charges N to `discarded_total`
    // rather than silently undercounting.
    let mut cursor = pending.clone();
    let mut lost: u64 = 1;
    for _ in 0..MAX_LOOKAHEAD {
        let probe = match with_spool(state, {
            let cursor = cursor.clone();
            move |spool| spool.peek_next(&cursor)
        })
        .await
        {
            Ok(Some(probe)) => probe,
            Ok(None) => {
                tracing::warn!(
                    %status,
                    lost,
                    "the collector has refused this record on enough separate attempts to \
                     discard it, but nothing later exists yet to prove the collector accepts \
                     anything else -- held, not discarded, until a new record arrives"
                );
                return Outcome::Stopped;
            }
            Err(error) => {
                tracing::error!(error = %error, "could not look for a record to probe the collector with");
                return Outcome::Stopped;
            }
        };

        if probe_accepted(state, &probe).await {
            let discarded = with_spool(state, {
                let pending = pending.clone();
                move |spool| spool.discard_confirmed(&pending, lost, &probe)
            })
            .await;
            return match discarded {
                Ok(()) => {
                    tracing::warn!(
                        %status,
                        lost,
                        "the collector has now refused this record on separate attempts and \
                         accepted a later one; discarding {lost} record(s) total"
                    );
                    Outcome::Advanced
                }
                Err(error) => {
                    tracing::error!(
                        error = %error,
                        lost,
                        "confirmed records were safe to discard but could not durably commit it \
                         -- they will be retried next attempt"
                    );
                    Outcome::Stopped
                }
            };
        }

        lost += 1;
        cursor = probe;
    }

    tracing::warn!(
        %status,
        lost,
        max_lookahead = MAX_LOOKAHEAD,
        "the collector refused this record on enough separate attempts, and every one of the \
         next {MAX_LOOKAHEAD} records was ALSO refused -- held, not discarded. This looks like \
         the collector itself is down, not a run of bad records; if it recovers, the next \
         attempt resumes from here"
    );
    Outcome::Stopped
}
