//! Whether the collector accepts a specific record, offered on its own.
//!
//! Split out of [`super::quarantine`] purely for the 200-LoC gate -- this is
//! one self-contained function with no state of its own, called in a loop by
//! [`super::quarantine::handle`]'s bounded lookahead.

use crate::otel_daemon::{
    DaemonState, forward, mint, normalize, receive::WireFormat, spool::Pending,
};

/// What offering `probe` to the collector, on its own, established.
///
/// Three outcomes, not two (PR #312 review, P1): a mint failure or a network
/// error is NOT the same evidence as an explicit refusal, even though both
/// once collapsed to a single `bool`. That collapse was safe for the
/// original one-hop design -- either one just meant "hold, try again later",
/// nothing was ever discarded on the strength of it. It stopped being safe
/// once [`super::quarantine::handle`]'s lookahead started walking PAST a
/// non-acceptance and discarding everything up to the record that finally
/// succeeds: an intermediate record whose probe merely timed out or hit a
/// mint hiccup would be swept into that discard with zero evidence it was
/// ever actually bad -- "unknown" reading as the *permissive* branch, which
/// `AGENTS.md`'s house rule on failure modes names as the exact thing to
/// never do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProbeOutcome {
    /// The collector took it -- proof it works.
    Accepted,
    /// The collector explicitly, permanently refused it (the same status
    /// class [`crate::copilot::is_permanent`] uses on the live path) -- real
    /// evidence this record is ALSO bad, safe to fold into a multi-record
    /// discard alongside the one already proven eligible.
    Refused,
    /// A mint failure, a normalize failure, or the collector being
    /// unreachable -- uninformative, exactly like the original design
    /// treated ANY non-acceptance. Never safe to discard on; only ever safe
    /// to hold and let a later attempt re-establish.
    Unknown,
}

/// Offers `probe` to the collector on its own, purely to learn whether it
/// accepts anything -- and, if not, whether that refusal is real evidence
/// or merely inconclusive. See [`ProbeOutcome`] for why the distinction
/// matters once a caller is deciding what to discard.
pub(super) async fn probe_outcome(state: &DaemonState, probe: &Pending) -> ProbeOutcome {
    let Ok(minted) = mint::mint(&state.http, &state.config).await else {
        return ProbeOutcome::Unknown;
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
        return ProbeOutcome::Unknown;
    };
    match forward::post(
        &state.http,
        &state.config,
        &minted.bearer,
        probe.signal,
        &stamped,
        is_json,
    )
    .await
    {
        Ok(crate::copilot::Verdict::Accepted) => ProbeOutcome::Accepted,
        Ok(crate::copilot::Verdict::Refused(_)) => ProbeOutcome::Refused,
        Err(_) => ProbeOutcome::Unknown,
    }
}
