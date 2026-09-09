//! Whether the collector accepts a specific record, offered on its own.
//!
//! Split out of [`super::quarantine`] purely for the 200-LoC gate -- this is
//! one self-contained function with no state of its own, called in a loop by
//! [`super::quarantine::handle`]'s bounded lookahead.

use crate::otel_daemon::{
    DaemonState, forward, mint, normalize, receive::WireFormat, spool::Pending,
};

/// Offers `probe` to the collector on its own, purely to learn whether it
/// accepts anything. `true` only on a clean `Accepted` -- a network error or
/// a mint failure is exactly as uninformative here as an explicit refusal:
/// none of them is evidence the collector works.
pub(super) async fn probe_accepted(state: &DaemonState, probe: &Pending) -> bool {
    let Ok(minted) = mint::mint(&state.http, &state.config).await else {
        return false;
    };
    // Mirrors `advance::advance_one`'s own detection: the record's ORIGINAL
    // wire format, not a re-sniff of its bytes -- a JSON body that happens to
    // also be valid protobuf (or vice versa) must still round-trip through
    // the same encoding it arrived in.
    let is_json = probe.format == WireFormat::Json;
    let parsed: Option<serde_json::Value> = is_json
        .then(|| serde_json::from_slice(&probe.payload).ok())
        .flatten();
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
