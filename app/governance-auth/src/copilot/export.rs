//! Getting one signal's records to the collector, including the ones it will
//! never take.
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
//! [`push::post`]) is answered by **splitting the batch in half and offering
//! each half**, down to single records. Halves rather than one-by-one because
//! a full drain can be thousands of records and 2·log₂(n) requests to isolate
//! a bad one is affordable where n requests is not.
//!
//! ## The rule that keeps a misconfiguration from emptying the spool
//!
//! A record is discarded only once the collector has **taken something else
//! from the same batch**. Without that rule, a collector misconfigured to 400
//! everything would be answered by discarding every record one at a time --
//! turning a five-minute config error into total, permanent data loss. With
//! it, "the collector refuses everything" is a plain failure that advances
//! nothing, and only a record its own siblings survived is given up on.
//!
//! The cost is that a batch of exactly one refused record stalls until
//! something else arrives to prove the collector is working. That is
//! self-healing rather than permanent: VS Code appends, the next batch has
//! acceptable records in it, and the bad one is isolated and dropped then.

use anyhow::{Result, bail};
use serde_json::Value;

use super::{
    batch,
    push::{self, Signal, Verdict},
    spool::Line,
};
use crate::redacted::Redacted;

/// Ceiling on the requests one signal's split may cost. A single bad record in
/// a full 8 MiB drain needs roughly two dozen; this leaves room for a handful
/// of them without letting a pathological batch hammer the collector. Hitting
/// it is a failure, not a licence to discard: the run stops, advances nothing,
/// and says so.
const MAX_REQUESTS: usize = 128;

#[derive(Debug, Default)]
pub struct Exported {
    /// Records of this signal the collector accepted.
    pub accepted: usize,
    /// Records it permanently refused, isolated one at a time. These are gone:
    /// the caller records them as discarded, and `status` shows the loss.
    pub refused: usize,
}

/// Offers `lines` to one signal's endpoint, isolating anything permanently
/// refused. `Ok` means the whole range is resolved -- every record either
/// delivered or given up on -- so the caller may advance that signal's offset.
pub async fn signal(
    http: &reqwest::Client,
    base: &str,
    signal: Signal,
    bearer: &Redacted<String>,
    lines: &[&Line],
) -> Result<Exported> {
    let Some((payload, records)) = build(lines, signal) else {
        // Nothing of this signal in this range: resolved, trivially.
        return Ok(Exported::default());
    };

    match push::post(http, base, signal, bearer, &payload).await? {
        Verdict::Accepted => Ok(Exported {
            accepted: records,
            refused: 0,
        }),
        Verdict::Refused(status) => {
            eprintln!(
                "The collector refused the {signal} batch with HTTP {status}. Splitting it to \
                 find the record(s) responsible -- retrying it unchanged would stop the drain at \
                 this byte offset permanently."
            );
            split(http, base, signal, bearer, lines).await
        }
    }
}

/// `None` when this range carries nothing for `signal`.
fn build(lines: &[&Line], signal: Signal) -> Option<(Value, usize)> {
    let (payload, records) = batch::build(lines).signal(signal);
    Some((payload?, records))
}

async fn split(
    http: &reqwest::Client,
    base: &str,
    signal: Signal,
    bearer: &Redacted<String>,
    lines: &[&Line],
) -> Result<Exported> {
    let mut pending = vec![(0usize, lines.len())];
    let mut done = Exported::default();
    let mut refused_ranges: Vec<usize> = Vec::new();
    let mut requests: usize = 0;

    while let Some((start, end)) = pending.pop() {
        let Some(slice) = lines.get(start..end) else {
            continue;
        };
        let Some((payload, records)) = build(slice, signal) else {
            continue;
        };

        requests = requests.saturating_add(1);
        if requests > MAX_REQUESTS {
            bail!(
                "splitting the refused {signal} batch cost more than {MAX_REQUESTS} requests \
                 without resolving it; stopping rather than continuing to hammer the collector. \
                 Nothing was advanced and nothing was discarded."
            );
        }

        match push::post(http, base, signal, bearer, &payload).await? {
            Verdict::Accepted => done.accepted = done.accepted.saturating_add(records),
            Verdict::Refused(_) if end.saturating_sub(start) <= 1 => refused_ranges.push(start),
            Verdict::Refused(_) => {
                let middle = start.saturating_add(end.saturating_sub(start) / 2);
                pending.push((start, middle));
                pending.push((middle, end));
            }
        }
    }

    if done.accepted == 0 && !refused_ranges.is_empty() {
        bail!(
            "the collector refused every {signal} record offered, individually. That is a \
             collector or configuration fault, not a bad record, so nothing was discarded and the \
             checkpoint did not move."
        );
    }

    done.refused = refused_ranges.len();
    for index in &refused_ranges {
        if let Some(line) = lines.get(*index) {
            // The offset, never the record: `AGENTS.md` bans logging a payload,
            // and this one is prompt-adjacent telemetry. A byte offset is
            // enough to find it -- the spool is never truncated.
            eprintln!(
                "Gave up on the {signal} record at byte {} of the spool; it is counted as \
                 discarded and `status` will show the loss.",
                line.offset
            );
        }
    }
    Ok(done)
}
