//! The bounded forward walk [`super::quarantine::handle`] uses once `pending`
//! is eligible for discard -- see that module's doc for the two-condition
//! design this is condition 2 of. Split out purely for the 200-LoC gate.
//!
//! ## Walking past more than one bad record
//!
//! [`super::spool::DurableSpool::peek_next`] only ever looks ONE record
//! ahead per call -- it has to, since deciding whether *that* record is
//! accepted means actually offering it to the collector, and a probe that
//! kept walking indefinitely on its own would turn one refusal into an
//! unbounded burst of credentialed requests. So [`walk`] is the one that
//! walks: if the immediate probe is EXPLICITLY refused
//! ([`super::probe::ProbeOutcome::Refused`]), it tries the record after
//! THAT, and so on, up to [`MAX_LOOKAHEAD`] records, rather than stopping at
//! the first refusal the way a single `peek_next` call would.
//!
//! An explicit refusal only -- not any non-acceptance. A mint failure or a
//! network error offering the probe ([`super::probe::ProbeOutcome::Unknown`])
//! is not evidence that record is bad; it is exactly as uninformative as it
//! always was for the original one-hop design, and the walk holds rather
//! than folding an unproven record into the discard (PR #312 review, P1 --
//! the first version of this fix collapsed both cases to one `bool` and
//! could silently lose a perfectly good record to a transient blip).
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
//!
//! ## One mint per walk, not one per probe
//!
//! [`mint::mint`] is minted ONCE below, before the loop, and reused across
//! every probe in this walk (PR #312 review, P2) -- it is `&self`-independent
//! and a wedged pass trying the full [`MAX_LOOKAHEAD`] would otherwise pay
//! for it up to 20 times. Safe even if the bearer goes stale partway through
//! a long walk: a collector that then answers 401 is not `is_permanent`, so
//! [`crate::otel_daemon::forward::post`] returns `Err`, which
//! [`probe::probe_outcome`] reports as [`ProbeOutcome::Unknown`] -- the walk
//! just holds early and the next pump tick re-mints fresh, the same
//! fail-safe outcome an expired token already produces anywhere else in this
//! daemon.

use super::{
    Outcome,
    probe::{self, ProbeOutcome},
    with_spool,
};
use crate::otel_daemon::{DaemonState, mint, spool::Pending};

/// How many records ahead of a stuck one this is willing to try before
/// concluding the collector itself is the problem, not a short run of bad
/// records. See `quarantine`'s module doc, "Walking past more than one bad
/// record".
pub(super) const MAX_LOOKAHEAD: usize = 20;

/// Walks forward from `pending` (already confirmed eligible by the caller)
/// looking for the first LATER record the collector actually accepts.
/// `cursor` is the record most recently tried; `lost` counts every record
/// given up on so far, `pending` included, so a run of N consecutive bad
/// records charges N to `discarded_total` rather than silently undercounting.
pub(super) async fn walk(
    state: &DaemonState,
    pending: Pending,
    status: axum::http::StatusCode,
) -> Outcome {
    // Minted once, up front, and reused for every probe below -- see the
    // module doc's "One mint per walk, not one per probe". A failure here is
    // exactly as uninformative as it would be per-probe: hold, don't discard.
    let Ok(minted) = mint::mint(&state.http, &state.config).await else {
        tracing::warn!(
            %status,
            "the collector refused this record on enough separate attempts, but no session was \
             available to probe the collector with -- held, not discarded, until it can be \
             tried again"
        );
        return Outcome::Stopped;
    };

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

        match probe::probe_outcome(state, &minted, &probe).await {
            ProbeOutcome::Accepted => {
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
                            "confirmed records were safe to discard but could not durably \
                             commit it -- they will be retried next attempt"
                        );
                        Outcome::Stopped
                    }
                };
            }
            // Real evidence this record is ALSO bad -- safe to fold into the
            // same discard once something later proves the collector, same
            // as `pending` itself already did via `record_refusal`.
            ProbeOutcome::Refused => {
                lost += 1;
                cursor = probe;
            }
            // Uninformative -- a mint failure, a network error, or the
            // collector being unreachable is not evidence THIS record is
            // bad (PR #312 review, P1). Discarding it anyway would be
            // "unknown" reading as the permissive branch, which is
            // backwards: hold, exactly as the original one-hop design held
            // on ANY non-acceptance, rather than walk past it.
            ProbeOutcome::Unknown => {
                tracing::warn!(
                    %status,
                    lost,
                    "the collector refused this record on enough separate attempts, but the \
                     next record's own probe was inconclusive (not an explicit refusal) -- \
                     held, not discarded, until it can be tried again"
                );
                return Outcome::Stopped;
            }
        }
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
