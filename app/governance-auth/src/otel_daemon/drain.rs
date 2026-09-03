//! Advancing the durable spool: the fail-closed `retain` itself, an
//! opportunistic drain that runs at the top of every accepted request, and a
//! background pump that keeps trying even when no client traffic arrives.
//!
//! ## Why two drivers, not one
//!
//! Draining only when a request arrives recovers instantly while traffic is
//! flowing, but AC2 ("an unreachable collector retains bytes; they are
//! delivered when it returns") does not say "once another request happens to
//! land" -- and AC1 (kill for an hour, restart, lose nothing) is explicitly
//! about a *restart*, which may see no traffic at all before the next
//! developer session. [`pump`] is what makes delivery happen on its own.
//!
//! Both drivers call the same [`advance_one`] step, so they cannot drift on
//! what counts as progress, a permanent refusal, or a stop.

use std::time::Duration;

use super::{DaemonState, forward, mint, normalize};
use crate::copilot::Verdict;

/// How often [`pump`] retries on its own, independent of client traffic.
/// Short enough that a backlog left by an outage clears within seconds of the
/// collector coming back; long enough not to hammer a collector that is
/// still down. Opportunistic draining (every accepted request) already
/// covers the case where traffic *is* flowing, so this constant only matters
/// when it is not.
const PUMP_INTERVAL: Duration = Duration::from_secs(5);

/// One attempt's outcome, for the two loops below to decide whether to keep
/// going.
enum Outcome {
    /// Nothing was pending.
    Empty,
    /// One record was delivered or given up on; there may be more.
    Advanced,
    /// Pending work exists but could not move past it this attempt (an
    /// unreachable collector, a refused mint, a first-time permanent
    /// refusal). Retrying immediately would spin.
    Stopped,
}

/// Retains a payload durably, surfacing a spool-full (fail-closed) loudly
/// rather than silently dropping it. The unavailable branch is the
/// restrictive branch; a full spool is reported, never papered over.
pub(super) fn retain(state: &DaemonState, signal: crate::copilot::Signal, payload: Vec<u8>) {
    let mut spool = state.spool.lock().unwrap_or_else(|p| p.into_inner());
    if let Err(error) = spool.retain(signal, payload) {
        tracing::error!(
            error = %error,
            "a payload could not be durably retained; this is a loss beyond the accepted spool \
             capacity"
        );
    }
}

/// One record's worth of mint -> stamp -> forward, against whatever the
/// durable spool currently has at the front.
async fn advance_one(state: &DaemonState) -> Outcome {
    let pending = {
        let mut spool = state.spool.lock().unwrap_or_else(|p| p.into_inner());
        match spool.next() {
            Ok(Some(pending)) => pending,
            Ok(None) => return Outcome::Empty,
            Err(error) => {
                tracing::error!(error = %error, "could not read the durable spool; stopping this pass");
                return Outcome::Stopped;
            }
        }
    };

    let minted = match mint::mint(&state.http, &state.config).await {
        Ok(minted) => minted,
        Err(error) => {
            tracing::warn!(error = %error, "no session; leaving the retained payload pending");
            return Outcome::Stopped;
        }
    };
    let stamped = match normalize::stamp(&pending.payload, &minted.access_token) {
        Ok(stamped) => stamped,
        Err(error) => {
            tracing::warn!(error = %error, "could not re-stamp a retained payload; leaving it pending");
            return Outcome::Stopped;
        }
    };

    match forward::post(
        &state.http,
        &state.config,
        &minted.bearer,
        pending.signal,
        &stamped,
    )
    .await
    {
        Ok(Verdict::Accepted) => {
            let mut spool = state.spool.lock().unwrap_or_else(|p| p.into_inner());
            if let Err(error) = spool.advance(&pending) {
                tracing::error!(
                    error = %error,
                    "delivered a retained payload but could not durably advance past it -- it \
                     may be re-delivered next attempt"
                );
            }
            Outcome::Advanced
        }
        Ok(Verdict::Refused(status)) => {
            let mut spool = state.spool.lock().unwrap_or_else(|p| p.into_inner());
            match spool.quarantine_or_discard(&pending) {
                Ok(true) => {
                    tracing::warn!(
                        %status,
                        "the collector has now refused this record on two separate attempts; \
                         discarding it"
                    );
                    Outcome::Advanced
                }
                Ok(false) => {
                    tracing::warn!(
                        %status,
                        "the collector refused this record; giving it one more separate attempt \
                         before discarding it"
                    );
                    Outcome::Stopped
                }
                Err(error) => {
                    tracing::error!(error = %error, "could not record a refusal");
                    Outcome::Stopped
                }
            }
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                "collector unreachable while draining the durable spool; leaving it pending"
            );
            Outcome::Stopped
        }
    }
}

/// Best-effort: runs at the top of every accepted request, advancing as far
/// as it can right now. Stops at the first `Empty` or `Stopped` so a request
/// is never held up spinning on an unreachable collector.
pub(super) async fn drain_retained(state: &DaemonState) {
    loop {
        match advance_one(state).await {
            Outcome::Advanced => {}
            Outcome::Empty | Outcome::Stopped => return,
        }
    }
}

/// Keeps retrying on [`PUMP_INTERVAL`] independent of client traffic. Runs
/// for the life of the daemon; [`super::serve`] aborts its task on shutdown.
pub(super) async fn pump(state: DaemonState) {
    let mut interval = tokio::time::interval(PUMP_INTERVAL);
    loop {
        interval.tick().await;
        drain_retained(&state).await;
    }
}
