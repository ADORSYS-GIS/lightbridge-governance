//! The in-memory spool retry: re-forwarding retained payloads once the
//! collector is healthy, and the fail-closed `retain` itself.
//!
//! [`drain_retained`] runs at the top of every accepted request ([`super`]'s
//! `handle_request`) and re-offers whatever the spool holds, stopping at the
//! first failure so it never spins on an unreachable collector. Because the
//! spool stores `(Signal, bytes)` ([`super::spool`]), each retry re-posts to the
//! same path the original forward used — which matters for a protobuf (non-JSON)
//! body, where the URL path is the only thing that names the signal.
//!
//! ## Three #290-review fixes live here
//!
//! - **P2-5**: one bearer is minted for the whole pass, not one per record.
//!   Minting per record meant N synchronous token round trips inside a
//!   single client-facing request for a backlog of N; `copilot_push`'s own
//!   drain already mints once per wake, and this now matches that
//!   convention.
//! - **P2-4 and the bot's "silent data drop" finding**: a record that could
//!   not be delivered this attempt is put back with
//!   [`super::spool::Spool::requeue_front`], not `retain` (which appends to
//!   the back). The old code re-`retain`d a drained record, which moved the
//!   oldest entry to the newest position on every failed retry — contradicting
//!   the "bounded FIFO" `spool`'s module doc claims — and did so through a
//!   fallible call whose `Err` was swallowed with `let _ =`, silently
//!   dropping the record if a concurrent request had refilled the freed
//!   capacity first. `requeue_front` is infallible (a payload this spool
//!   already held once cannot make the total grow), so there is no `Err` left
//!   to swallow.
//! - **P2-6**: a permanent refusal (`is_permanent`: 400/413/422) is
//!   discarded, not retried. `is_permanent` means the collector will never
//!   accept these bytes "however many times they are offered"
//!   (`copilot::push`'s own words) — retaining it anyway monotonically fills
//!   the spool with payloads that can never drain, and once full every *new*
//!   payload is refused too. `copilot_push`'s drain isolates and gives up on
//!   such a record; this does the same, simplified to "give up immediately"
//!   rather than that drain's two-separate-wakes quarantine (#269 adds that
//!   sophistication once it lands on top of this).

use super::{DaemonState, forward, mint, normalize};
use crate::copilot::Signal;

/// Retains a payload on the signal it routes on, surfacing a spool-full
/// (fail-closed) loudly rather than silently dropping. Returns whether the
/// payload actually made it into the spool: the caller must not answer the
/// client `202` when this is `false` (#290 review, P1-3) -- a failed retain
/// reported as accepted is the exporter being told "delivered" while its
/// only copy is dropped, which is the unavailable branch becoming the
/// permissive one.
#[must_use]
pub(super) fn retain(state: &DaemonState, signal: Signal, payload: Vec<u8>) -> bool {
    let mut spool = state.spool.lock().unwrap_or_else(|p| p.into_inner());
    match spool.retain(signal, payload) {
        Ok(()) => true,
        Err(error) => {
            tracing::error!(error = %error, pending = spool.pending(), "payload could not be retained because the spool is full; this is a loss beyond the accepted #268 in-memory window");
            false
        }
    }
}

/// Best-effort drain of retained payloads. Stops at the first retryable
/// failure so we do not spin on an unreachable collector; a permanent
/// refusal is discarded and draining continues (P2-6, above).
pub(super) async fn drain_retained(state: &DaemonState) {
    // One mint for the whole pass -- see the module doc's P2-5. A failed
    // mint here means no record in the backlog is authenticatable right now
    // regardless of which one it is, so there is nothing a per-record retry
    // would gain.
    let minted = match mint::mint(&state.http, &state.config).await {
        Ok(minted) => minted,
        Err(_) => return,
    };

    loop {
        let (signal, payload) = {
            let mut spool = state.spool.lock().unwrap_or_else(|p| p.into_inner());
            match spool.drain_one() {
                Some(item) => item,
                None => return,
            }
        };

        let stamped = match normalize::stamp(&payload, &minted.access_token) {
            Ok(stamped) => stamped,
            Err(error) => {
                tracing::warn!(error = %error, "could not re-stamp a retained payload; requeuing it");
                let mut spool = state.spool.lock().unwrap_or_else(|p| p.into_inner());
                spool.requeue_front(signal, payload);
                return;
            }
        };

        match forward::post(&state.http, &state.config, &minted.bearer, signal, &stamped).await {
            Ok(forward::Verdict::Accepted) => {}
            Ok(forward::Verdict::Refused(status)) => {
                tracing::error!(
                    %status,
                    "the collector permanently refused a retained payload; discarding it, not \
                     retrying forever"
                );
            }
            Err(error) => {
                tracing::warn!(error = %error, "collector unreachable while draining; requeuing the payload");
                let mut spool = state.spool.lock().unwrap_or_else(|p| p.into_inner());
                spool.requeue_front(signal, stamped);
                return;
            }
        }
    }
}
