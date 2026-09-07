//! Advancing the durable spool: the fail-closed `retain` itself, an
//! opportunistic drain that runs at the top of every accepted request, and a
//! background pump that keeps trying even when no client traffic arrives.
//!
//! ## Why two drivers, not one
//!
//! Draining only on a request recovers instantly while traffic flows, but
//! AC2 (an unreachable collector delivers once it returns) does not say
//! "once another request happens to land", and AC1 (kill for an hour,
//! restart, lose nothing) is about a *restart*, which may see no traffic
//! before the next developer session. [`pump`] makes delivery happen on its
//! own. Both drivers call the same [`advance::advance_one`], so they cannot
//! drift on what counts as progress, a permanent refusal, or a stop.
//!
//! ## Every spool operation runs off the async runtime (P2-7)
//!
//! [`spool::DurableSpool`]'s methods are all synchronous file I/O -- a read,
//! an `O_APPEND` write, an `fsync`, a tmp-then-rename -- none of which
//! yields, so running one inline on a tokio worker thread blocks whatever
//! else that thread was scheduled to run for however long the filesystem
//! takes. [`with_spool`] is the one seam every spool access in this module
//! goes through, off that thread.
//!
//! ## A request-triggered pass is bounded (#269/#291 review round 2, P2)
//!
//! [`drain_retained`] used to loop until the spool was empty or stalled --
//! fine for [`pump`], which owns no client, but [`super::handle_request`]
//! calls it before answering one: a full 16 MiB backlog at ~3 KiB/record is
//! ~5,000 mint+POST round trips, all inside a request nothing asked to wait
//! that long. [`DRAIN_BUDGET_PER_PASS`] caps one call's work; `pump`'s own
//! timer keeps calling this regardless, so a capped pass does not stall a
//! large backlog -- it spreads draining it across several ticks instead of
//! blocking one caller with all of it.

mod advance;
mod quarantine;

use std::time::Duration;

use super::{DaemonState, spool};

/// How often [`pump`] retries on its own, independent of client traffic.
/// Short enough that a backlog clears within seconds of the collector coming
/// back; long enough not to hammer one still down. Opportunistic draining
/// already covers the case where traffic *is* flowing.
const PUMP_INTERVAL: Duration = Duration::from_secs(5);

/// Most records one call to [`drain_retained`] advances before returning,
/// even when more remain -- see the module doc's P2 section.
const DRAIN_BUDGET_PER_PASS: usize = 32;

/// One attempt's outcome, for the two loops below to decide whether to keep
/// going.
pub(super) enum Outcome {
    /// Nothing was pending.
    Empty,
    /// One record was delivered or given up on; there may be more.
    Advanced,
    /// Pending work exists but could not move past it this attempt.
    /// Retrying immediately would spin.
    Stopped,
}

/// Runs `f` against the spool on a blocking-pool thread -- see the module
/// doc's P2-7 section. A panic inside `f` is resumed, not swallowed by
/// `spawn_blocking`'s own `JoinError`, so it surfaces as it would inline.
async fn with_spool<T, F>(state: &DaemonState, f: F) -> T
where
    F: FnOnce(&mut spool::DurableSpool) -> T + Send + 'static,
    T: Send + 'static,
{
    let spool = state.spool.clone();
    tokio::task::spawn_blocking(move || {
        let mut spool = spool.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut spool)
    })
    .await
    .unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()))
}

/// Retains a payload durably, answering whether it durably landed -- `false`
/// surfaces a spool-full (or any other write) failure loudly rather than
/// silently dropping it: the caller must turn this into `503`, never a
/// `202` the payload never earned.
pub(super) async fn retain(
    state: &DaemonState,
    signal: crate::copilot::Signal,
    payload: Vec<u8>,
) -> bool {
    let ok = with_spool(state, move |spool| spool.retain(signal, payload)).await;
    match ok {
        Ok(()) => true,
        Err(error) => {
            tracing::error!(
                error = %error,
                "a payload could not be durably retained; this is a loss beyond the accepted \
                 spool capacity"
            );
            false
        }
    }
}

/// Best-effort: runs at the top of every accepted request, advancing as far
/// as [`DRAIN_BUDGET_PER_PASS`] allows. Stops early at the first `Empty` or
/// `Stopped` so a request is never held up spinning on an unreachable
/// collector. Peeks [`spool::DurableSpool::is_empty`] first, so the common
/// case -- nothing pending -- costs a blocking-pool round trip to check a
/// size, not a mint.
pub(super) async fn drain_retained(state: &DaemonState) {
    for _ in 0..DRAIN_BUDGET_PER_PASS {
        match with_spool(state, |spool| spool.is_empty()).await {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                tracing::error!(error = %error, "could not check whether the durable spool is empty; stopping this pass");
                return;
            }
        }
        match advance::advance_one(state).await {
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
