//! What happens to a record the collector refused **on its own**, once the
//! split has narrowed down to it.
//!
//! Split out of [`super`] to keep both halves under the 200-LoC gate, and
//! because this is the one decision in the drain that destroys data: it is
//! worth reading without the traversal around it.
//!
//! ## Two conditions, and neither is sufficient alone
//!
//! 1. **Refused across separate wakes** ([`super::super::quarantine`]). A 400
//!    is a deterministic function of the payload only when nothing sits in
//!    front of the collector; a WAF, a proxy or an upstream restart returns
//!    one for reasons of its own, and a drain that trusted a single 400
//!    deleted four valid records in one measured round.
//! 2. **The collector has been shown to accept something.** Otherwise a
//!    collector misconfigured to refuse everything is answered by discarding
//!    the spool one record per wake -- a five-minute config error turned into
//!    permanent, total data loss.
//!
//! ## Why condition 2 sometimes needs a probe
//!
//! Usually condition 2 is free: the split delivered records before reaching
//! this one, so the collector has visibly just worked. But once the offset has
//! advanced up to a bad record, that record is the **first** thing offered on
//! every later wake -- nothing precedes it, so there is no free proof, and
//! nothing after it may simply be delivered instead (the offset is a
//! high-water mark, so an out-of-order delivery is a duplicate next wake).
//! Without an answer, such a record stalls the stream permanently, which is
//! the poison pill this whole module exists to remove.
//!
//! So when condition 1 is already met and condition 2 is not, the next record
//! carrying this signal is offered **on its own** as a probe:
//!
//! - **Accepted** -- the collector works. The bad record is discarded, and the
//!   resolved prefix covers the probe too, so its delivery is recorded in the
//!   same wake it happened. No duplicate.
//! - **Refused** -- the collector is refusing everything. Nothing is
//!   discarded, nothing advances, and the wake stops. Costs one request.
//!
//! There is deliberately no fallback to "the checkpoint says we pushed
//! successfully an hour ago". That answer is cheap and wrong in exactly the
//! case condition 2 exists for: a collector that worked this morning and
//! rejects everything now.

use anyhow::anyhow;

use super::{Offer, Progress, build};
use crate::copilot::{
    push::{self, Verdict},
    quarantine::{self, Quarantine},
    spool::Line,
};

/// `index` is the position of the refused record in `lines`. Returns `true`
/// when the pass may carry on past it.
pub(super) async fn refused(
    offer: &mut Offer<'_>,
    progress: &mut Progress,
    lines: &[&Line],
    index: usize,
) -> bool {
    let Some(line) = lines.get(index) else {
        return false;
    };
    let key = Quarantine::key(&line.text);
    let enough_wakes = offer.quarantine.refused(&key, offer.now);
    let resolved_to = if !enough_wakes {
        None
    } else if progress.accepted > 0 {
        Some(index.saturating_add(1))
    } else {
        probe(offer, progress, lines, index).await
    };

    match resolved_to {
        Some(end) => {
            offer.quarantine.forget(&key);
            progress.discarded = progress.discarded.saturating_add(1);
            progress.resolved = end;
            // The offset, never the record: `AGENTS.md` bans logging a payload,
            // and this one is prompt-adjacent telemetry. A byte offset is
            // enough to find it -- the spool is never truncated.
            eprintln!(
                "Gave up on the {} record at byte {} of the spool after {} separate wakes refused \
                 it; it is counted as discarded and `status` will show the loss.",
                offer.signal,
                line.offset,
                quarantine::REFUSALS_BEFORE_DISCARD
            );
            true
        }
        None => {
            progress.held = progress.held.saturating_add(1);
            progress.stopped = Some(anyhow!(
                "the collector refused the {} record at byte {} of the spool on its own. It is \
                 held, not discarded: {}. Everything before it was delivered and the offset moved.",
                offer.signal,
                line.offset,
                if enough_wakes {
                    "this collector has not accepted anything, which is a collector or \
                     configuration fault rather than a bad record"
                } else {
                    "a single wake's 400 can come from a proxy rather than from the payload, so it \
                     takes a second wake to agree"
                }
            ));
            false
        }
    }
}

/// Offers the next record carrying this signal on its own, purely to find out
/// whether the collector accepts anything. On acceptance it returns the prefix
/// length that is now resolved -- **including the probe**, so the caller's
/// offset moves past a record that has just been delivered rather than
/// re-sending it next wake.
async fn probe(
    offer: &mut Offer<'_>,
    progress: &mut Progress,
    lines: &[&Line],
    index: usize,
) -> Option<usize> {
    for next in index.saturating_add(1)..lines.len() {
        let slice = lines.get(next..next.saturating_add(1))?;
        let Some((payload, records)) = build(slice, offer.signal) else {
            continue; // carries nothing for this signal
        };
        progress.requests = progress.requests.saturating_add(1);
        return match push::post(offer.http, offer.base, offer.signal, offer.bearer, &payload).await
        {
            Ok(Verdict::Accepted) => {
                progress.accepted = progress.accepted.saturating_add(records);
                Some(next.saturating_add(1))
            }
            // Refused, or the collector went away mid-probe: either way this
            // is not proof that it accepts anything.
            _ => None,
        };
    }
    // Nothing else to offer. A batch of exactly one refused record waits until
    // something acceptable arrives beside it -- self-healing, not stuck.
    None
}
