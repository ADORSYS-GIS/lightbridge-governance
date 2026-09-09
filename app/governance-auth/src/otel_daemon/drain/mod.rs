//! Advancing the durable spool: the fail-closed `retain` itself and the one
//! background consumer that forwards retained records.
//!
//! ## Why one driver, not two
//!
//! A timer alone recovers without client traffic but adds avoidable latency;
//! request-triggered drains recover promptly but can race the timer after
//! reading the same checkpoint. [`pump`] is therefore the sole consumer:
//! [`retain`] wakes it immediately, and its timer retries stalled work even
//! when no new request arrives. One owner makes the read -> POST -> checkpoint
//! sequence serial without holding a filesystem mutex across network I/O.
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
//! ## Each pass is bounded (#269/#291 review round 2, P2)
//!
//! [`drain_retained`] used to loop until the spool was empty or stalled --
//! a full 16 MiB backlog at ~3 KiB/record can be ~5,000 mint+POST round trips.
//! [`DRAIN_BUDGET_PER_PASS`] gives the executor a scheduling boundary. A pass
//! that uses the full budget continues immediately rather than waiting for
//! the timer, so the boundary does not impose a throughput ceiling.

mod advance;
mod probe;
mod quarantine;

use std::time::Duration;

use super::{DaemonState, spool};

/// How often [`pump`] retries on its own, independent of client traffic.
/// Short enough that a backlog clears within seconds of the collector coming
/// back; long enough not to hammer one still down. A successful admission
/// wakes the pump immediately when it is idle.
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

enum Pass {
    Idle,
    Stopped,
    BudgetExhausted,
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
/// success the payload never earned.
pub(super) async fn retain(
    state: &DaemonState,
    signal: crate::copilot::Signal,
    payload: Vec<u8>,
    format: super::receive::WireFormat,
) -> bool {
    let ok = with_spool(state, move |spool| spool.retain(signal, payload, format)).await;
    match ok {
        Ok(()) => {
            state.drain_wake.notify_one();
            true
        }
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

/// Advances as far as [`DRAIN_BUDGET_PER_PASS`] allows. Stops early at the
/// first `Empty` or `Stopped` so the pump never spins on an unreachable
/// collector. Peeks [`spool::DurableSpool::is_empty`] first, so the common
/// case costs a blocking-pool round trip to check a size, not a mint.
async fn drain_retained(state: &DaemonState) -> Pass {
    for _ in 0..DRAIN_BUDGET_PER_PASS {
        match with_spool(state, |spool| spool.is_empty()).await {
            Ok(true) => return Pass::Idle,
            Ok(false) => {}
            Err(error) => {
                tracing::error!(error = %error, "could not check whether the durable spool is empty; stopping this pass");
                return Pass::Stopped;
            }
        }
        match advance::advance_one(state).await {
            Outcome::Advanced => {}
            Outcome::Empty => return Pass::Idle,
            Outcome::Stopped => return Pass::Stopped,
        }
    }
    Pass::BudgetExhausted
}

/// The spool's single consumer. A successful retain wakes it immediately;
/// the timer retries stalled work after backoff even when no new telemetry
/// arrives. A full productive pass continues immediately, so throughput is
/// not capped at 32 records per five seconds. Keeping one consumer also
/// prevents independent triggers from forwarding the same checkpoint
/// concurrently.
pub(super) async fn pump(state: DaemonState) {
    let mut interval = tokio::time::interval(PUMP_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Consume `interval`'s immediate first tick. The spool is checked below,
    // and successful admission supplies an explicit wake.
    interval.tick().await;
    loop {
        match drain_retained(&state).await {
            Pass::BudgetExhausted => continue,
            Pass::Idle => {
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = state.drain_wake.notified() => {}
                }
            }
            // New arrivals cannot make a refused credential or collector
            // healthy, so traffic must not defeat this retry backoff.
            Pass::Stopped => {
                interval.tick().await;
            }
        }
    }
}
