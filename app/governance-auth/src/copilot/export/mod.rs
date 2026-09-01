//! Getting one signal's records to the collector, including the ones it will
//! never take -- and never re-offering one it already took.
//!
//! ## The poison pill this exists to remove
//!
//! The checkpoint only moves after a 2xx. That is right for a collector that
//! is down, or slow, or rejecting a token -- the bytes stay pending and go
//! again next wake. It is exactly wrong for a payload the collector will
//! *never* accept: the same bytes rebuild the same refused payload on every
//! wake, forever, and every record written after it is stuck behind one that
//! is never going anywhere. A drain that cannot get past a bad record does not
//! lose one record; it eventually loses all of them.
//!
//! So a refusal ([`push::Verdict::Refused`] -- 400/413/422 only, see
//! [`push::post`]) is answered by **splitting the range in half and offering
//! each half**, down to single records. Halves rather than one-by-one because
//! a full drain can be thousands of records and 2·log₂(n) requests to isolate
//! a bad one is affordable where n requests is not.
//!
//! ## Why the split walks strictly left to right
//!
//! ⚠️ This is the part that was wrong, and it was worse than what it replaced.
//! The split used to visit ranges in whatever order a stack produced and
//! report only totals, so the sub-batches the collector **accepted** on the
//! way down were recorded nowhere. Run out of the request budget, or lose the
//! connection mid-split, and the whole signal returned `Err`: no offset moved,
//! and the next wake rebuilt and re-sent every record already delivered.
//! Measured at 512 records with 12 refused -- 438 good records duplicated on
//! every wake, for ever, into a usage store.
//!
//! Progress is a single high-water byte offset, so the only thing it can
//! express is "everything up to here is done". The traversal below is
//! therefore an in-order DFS: the two halves are pushed right-then-left so the
//! left pops first, which makes the completed ranges a contiguous **prefix**
//! at every instant. [`Progress::resolved`] is that prefix, and the caller
//! advances the offset over it whether the pass finished or stopped short.
//! Stopping is then cheap and safe -- it costs a wake, never a duplicate.
//!
//! ## The two rules that keep a misconfiguration from emptying the spool
//!
//! 1. A record is given up on only once it has been refused on its own across
//!    separate wakes -- see [`quarantine`], and finding 2 of the audit that
//!    put it there: a gateway answering 400 for reasons of its own had four
//!    valid records permanently discarded, exit 0.
//! 2. And only once the collector has been shown, *in this pass*, to accept
//!    something. A collector misconfigured to refuse everything has accepted
//!    nothing, so it advances nothing and discards nothing. See [`isolate`]
//!    for the one-request probe that answers this when the bad record is at
//!    the very front and there is nothing before it to prove anything with.
//!
//! A refusal that satisfies neither condition **stops the pass** rather than
//! skipping past it. That is not a stall: the prefix before it has already
//! been delivered and recorded, and the next wake resolves it.

//!
//! ## And why the offset moves during the pass, not after it
//!
//! The prefix is recorded the moment it advances, because the wake may not get
//! to have an "after": a `TimeoutStartSec=` kill, an OOM, a closed lid. See
//! [`super::journal`].

mod isolate;
mod progress;

use anyhow::anyhow;
pub use progress::{Offer, Progress};
use progress::{build, commit};

use super::{
    push::{self, Verdict},
    spool::Line,
};

/// Ceiling on the requests one signal's pass may cost.
///
/// ⚠️ Unlike the cap this replaced, hitting it is **not** a cliff: the pass
/// stops, the caller advances over [`Progress::resolved`], and the next wake
/// continues from there. The old 128-request cap threw the whole pass away
/// instead, which is how it turned a stall into a stall *plus* unbounded
/// duplicate delivery.
const MAX_REQUESTS: usize = 512;

/// Offers `lines` to one signal's endpoint, isolating anything permanently
/// refused, and reports how far it got.
pub async fn signal(offer: &mut Offer<'_, '_>, lines: &[&Line]) -> Progress {
    let mut progress = Progress::default();

    // In-order DFS. Right half pushed first so the left half pops first: that
    // is what makes the completed ranges a contiguous prefix -- see the
    // module doc.
    let mut pending = vec![(0usize, lines.len())];
    while let Some((start, end)) = pending.pop() {
        // At the top rather than the bottom so that every path out of the body
        // below -- including the two `continue`s and the four `break`s -- is
        // followed by a commit, either on the next turn or by the one after
        // the loop. A pass whose prefix has not moved writes nothing.
        if let Err(error) = commit(offer, &mut progress, lines) {
            progress.stopped = Some(error);
            break;
        }
        // ⚠️ A range the prefix already covers must not be offered again.
        // [`isolate`]'s probe resolves a record *ahead* of the one being
        // isolated, so the prefix can jump past ranges still on the stack --
        // and re-posting one of those delivers a record the collector already
        // took. Clamping rather than skipping outright, because a stacked
        // range can be only partly covered.
        let start = start.max(progress.resolved);
        if start >= end {
            continue;
        }
        if progress.requests >= MAX_REQUESTS {
            progress.stopped = Some(anyhow!(
                "splitting the refused {} batch reached this wake's budget of {MAX_REQUESTS} \
                 requests. The {} record(s) already delivered are recorded and the next wake \
                 resumes from there.",
                offer.signal,
                progress.accepted
            ));
            break;
        }
        let Some(slice) = lines.get(start..end) else {
            progress.stopped = Some(anyhow!(
                "internal error: range {start}..{end} is out of bounds"
            ));
            break;
        };
        let Some((payload, records)) = build(slice, offer.signal) else {
            // Nothing of this signal in this range: resolved, trivially.
            progress.resolved = end;
            continue;
        };

        progress.requests = progress.requests.saturating_add(1);
        match push::post(offer.http, offer.base, offer.signal, offer.bearer, &payload).await {
            Err(error) => {
                progress.stopped = Some(error);
                break;
            }
            Ok(Verdict::Accepted) => {
                progress.accepted = progress.accepted.saturating_add(records);
                progress.resolved = end;
            }
            Ok(Verdict::Refused(status)) if end.saturating_sub(start) > 1 => {
                if start == 0 && end == lines.len() {
                    eprintln!(
                        "The collector refused the {} batch with HTTP {status}. Splitting it to \
                         find the record(s) responsible -- retrying it unchanged would stop the \
                         drain at this byte offset permanently.",
                        offer.signal
                    );
                }
                let middle = start.saturating_add(end.saturating_sub(start) / 2);
                pending.push((middle, end));
                pending.push((start, middle));
            }
            Ok(Verdict::Refused(_)) => {
                if !isolate::refused(offer, &mut progress, lines, start).await {
                    break;
                }
            }
        }
    }
    // However the pass ended, what it resolved is durable before it returns.
    // `get_or_insert` because a commit that already failed above is the more
    // useful of the two errors -- the retry here fails for the same reason.
    if let Err(error) = commit(offer, &mut progress, lines) {
        progress.stopped.get_or_insert(error);
    }
    progress
}
