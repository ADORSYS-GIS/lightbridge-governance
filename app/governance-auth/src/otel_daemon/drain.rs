//! The in-memory spool retry: re-forwarding retained payloads once the
//! collector is healthy, and the fail-closed `retain` itself.
//!
//! [`drain_retained`] runs at the top of every accepted request ([`super`]'s
//! `handle_request`) and re-offers whatever the spool holds, stopping at the
//! first failure so it never spins on an unreachable collector. Because the
//! spool stores `(Signal, bytes)` ([`super::spool`]), each retry re-posts to the
//! same path the original forward used — which matters for a protobuf (non-JSON)
//! body, where the URL path is the only thing that names the signal.

use super::{DaemonState, forward, mint, normalize};
use crate::copilot::Signal;

/// Retains a payload on the signal it routes on, surfacing a spool-full
/// (fail-closed) loudly rather than silently dropping. The unavailable branch
/// is the restrictive branch; a full spool is reported, never papered over.
pub(super) fn retain(state: &DaemonState, signal: Signal, payload: Vec<u8>) {
    let mut spool = state.spool.lock().unwrap_or_else(|p| p.into_inner());
    if let Err(error) = spool.retain(signal, payload) {
        tracing::error!(error = %error, pending = spool.pending(), "payload could not be retained because the spool is full; this is a loss beyond the accepted #268 in-memory window");
    }
}

/// Best-effort drain of retained payloads. Stops at the first failure so we
/// do not spin on an unreachable collector.
pub(super) async fn drain_retained(state: &DaemonState) {
    loop {
        let (signal, payload) = {
            let mut spool = state.spool.lock().unwrap_or_else(|p| p.into_inner());
            match spool.drain_one() {
                Some((signal, payload)) => (signal, payload),
                None => return,
            }
        };
        let minted = match mint::mint(&state.http, &state.config).await {
            Ok(minted) => minted,
            Err(_) => {
                let mut spool = state.spool.lock().unwrap_or_else(|p| p.into_inner());
                let _ = spool.retain(signal, payload);
                return;
            }
        };
        let stamped = match normalize::stamp(&payload, &minted.access_token) {
            Ok(stamped) => stamped,
            Err(_) => {
                let mut spool = state.spool.lock().unwrap_or_else(|p| p.into_inner());
                let _ = spool.retain(signal, payload);
                return;
            }
        };
        match forward::post(&state.http, &state.config, &minted.bearer, signal, &stamped).await {
            Ok(forward::Verdict::Accepted) => {}
            Ok(forward::Verdict::Refused(_)) | Err(_) => {
                let mut spool = state.spool.lock().unwrap_or_else(|p| p.into_inner());
                let _ = spool.retain(signal, stamped);
                return;
            }
        }
    }
}
