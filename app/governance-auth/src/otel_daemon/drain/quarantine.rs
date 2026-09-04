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
//! - **Accepted** -- the collector works. The stuck record is discarded, and
//!   the probe's own delivery is committed in the same write
//!   ([`super::spool::DurableSpool::discard_confirmed`]), so it is never
//!   re-offered as if it were still pending.
//! - **Refused, or the collector is unreachable** -- not proof. Nothing is
//!   discarded, nothing advances.
//! - **Nothing exists yet past the stuck record** -- there is nothing to
//!   prove the collector with. Held, not discarded; this is the one stall
//!   that does not resolve itself on `pump`'s timer -- it clears once a new
//!   record is retained, and not before.
//!
//! There is deliberately no fallback to "the checkpoint says a forward
//! succeeded a minute ago". That answer is cheap and wrong in exactly the
//! case condition 2 exists for: a collector that worked a minute ago and
//! refuses everything now.

use super::{Outcome, with_spool};
use crate::otel_daemon::{DaemonState, forward, mint, normalize, spool::Pending};

/// Handles a `Verdict::Refused` outcome for `pending`: records the refusal,
/// and -- only once it is both eligible AND confirmed, per the module doc --
/// discards it.
pub(super) async fn handle(
    state: &DaemonState,
    pending: Pending,
    status: axum::http::StatusCode,
) -> Outcome {
    let eligible = match with_spool(state, {
        let pending = pending.clone();
        move |spool| spool.record_refusal(&pending)
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

    let probe = match with_spool(state, {
        let pending = pending.clone();
        move |spool| spool.peek_next(&pending)
    })
    .await
    {
        Ok(Some(probe)) => probe,
        Ok(None) => {
            tracing::warn!(
                %status,
                "the collector has refused this record on enough separate attempts to discard \
                 it, but nothing later exists yet to prove the collector accepts anything else \
                 -- held, not discarded, until a new record arrives"
            );
            return Outcome::Stopped;
        }
        Err(error) => {
            tracing::error!(error = %error, "could not look for a record to probe the collector with");
            return Outcome::Stopped;
        }
    };

    if !probe_accepted(state, &probe).await {
        tracing::warn!(
            %status,
            "the collector refused this record on enough separate attempts, but also refused \
             the probe meant to confirm it accepts anything else -- held, not discarded"
        );
        return Outcome::Stopped;
    }

    let discarded = with_spool(state, move |spool| {
        spool.discard_confirmed(&pending, &probe)
    })
    .await;
    match discarded {
        Ok(()) => {
            tracing::warn!(
                %status,
                "the collector has now refused this record on separate attempts and accepted a \
                 later one; discarding it"
            );
            Outcome::Advanced
        }
        Err(error) => {
            tracing::error!(
                error = %error,
                "confirmed a record was safe to discard but could not durably commit it -- it \
                 will be retried next attempt"
            );
            Outcome::Stopped
        }
    }
}

/// Offers `probe` to the collector on its own, purely to learn whether it
/// accepts anything. `true` only on a clean `Accepted` -- a network error or
/// a mint failure is exactly as uninformative here as an explicit refusal:
/// none of them is evidence the collector works.
async fn probe_accepted(state: &DaemonState, probe: &Pending) -> bool {
    let Ok(minted) = mint::mint(&state.http, &state.config).await else {
        return false;
    };
    let parsed: Option<serde_json::Value> = serde_json::from_slice(&probe.payload).ok();
    let is_json = parsed.is_some();
    let Ok(stamped) = normalize::stamp(
        parsed,
        &probe.payload,
        &minted.access_token,
        Some(&probe.key),
    ) else {
        return false;
    };
    matches!(
        forward::post(
            &state.http,
            &state.config,
            &minted.bearer,
            probe.signal,
            &stamped,
            is_json,
        )
        .await,
        Ok(crate::copilot::Verdict::Accepted)
    )
}
