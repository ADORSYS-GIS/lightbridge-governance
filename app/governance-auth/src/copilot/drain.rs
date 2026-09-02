//! One wake: take the lock, then sweep the spool until it is caught up or a
//! bound says stop.
//!
//! [`super::sweep`] is one read -> export -> checkpoint pass and owns everything about
//! how a pass behaves. This module owns only how many of them a wake makes, and
//! why that is not one.
//!
//! ## Why a wake is a loop
//!
//! [`super::spool`] reads at most `MAX_READ` = 8 MiB per call, which bounds the
//! `Vec<u8>` it reads into. Until this loop existed that also bounded the
//! *wake*, because a wake was exactly one sweep. Measured on a maintainer's
//! Linux desktop, 2026-09-02, from two `status` calls either side of one wake:
//!
//! - spool 164 MB, pending before 155,755,698 bytes, pending after 147,370,638
//! - so **8,385,060 bytes in one wake** -- 8 MiB, cut back to the last complete
//!   line -- at one wake per 300 s: **27 KB/s**
//! - 147 MB of backlog is then ~18 wakes, ≈1.5 hours, and only if Copilot
//!   writes nothing more. Above 27 KB/s of writing it never catches up at all.
//!
//! That is not only slow, it is self-defeating: [`super::spool::reclaim`] fires only
//! when `size == offset`, so the spools with the most to reclaim were the ones
//! whose backlog guaranteed the precondition could never be met. The machines
//! that most needed the file bounded waited longest for it. With the loop, the
//! sweep that finishes the backlog is the one holding `size == offset`, and the
//! reclaim fires in that same wake -- 164 MB drained and truncated in one wake
//! where a fast collector allows, and in a handful otherwise.
//!
//! Raising `MAX_READ` instead was rejected: it trades memory for throughput and
//! still leaves a fixed ceiling per wake, so it does not fix the general case.
//! Looping leaves peak memory exactly where it was -- one 8 MiB read at a time,
//! the previous sweep's lines dropped before the next is read.
//!
//! ## The three bounds, and why a wake needs all three
//!
//! 1. **[`Swept::complete`]** -- correctness, not throttling. A sweep that did
//!    not resolve everything it read ends the wake, because sweeping again
//!    would re-offer records this wake has already offered and charge
//!    [`super::quarantine`] two refusals for one wake's evidence. It also makes
//!    every continued sweep read strictly new bytes, so the offset advances
//!    strictly and the loop cannot spin on a sweep that delivers nothing.
//! 2. **[`BUDGET`]** -- wall clock, checked between sweeps.
//! 3. **[`MAX_PASSES`]** -- a count that does not depend on how fast the
//!    machine or the collector is.
//!
//! ⚠️ Both 2 and 3 are in-process on purpose. systemd's `TimeoutStartSec=240`
//! (`schedule::systemd`) SIGKILLs an overrunning wake, but launchd has no
//! equivalent at all -- `schedule::launchd` and
//! `docs/governance-auth/commands.md` both spell that out -- so on macOS an
//! in-process bound is the *only* bound there is.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::Result;

use super::{
    checkpoint, lock,
    sweep::{self, Swept, Wake},
};
use crate::redacted::Redacted;

/// Wall clock the sweeps may take, checked *between* them, so the true ceiling
/// is this plus one sweep.
///
/// 60 s answers four separate constraints at once, and it is the smallest of
/// them that decides it:
///
/// - a quarter of systemd's `TimeoutStartSec=240`, leaving room for the
///   authentication that precedes the drain and for one overrunning final sweep
///   (a sweep that bisects a refused batch may make up to 512 requests per
///   signal -- an exposure that predates this loop and that no budget here can
///   shrink, since the check happens between sweeps);
/// - half of `lock::HELD_BY_A_LIVE_DRAIN`, the 120 s a *second* drain waits
///   before giving up on the lock. The wake holds `copilot-push.lock`
///   throughout, and `status` tells developers to run `copilot-push` by hand
///   exactly when there is a backlog, so the hand-run has to still get in;
/// - a fifth of the 300 s timer interval, so wakes cannot queue;
/// - and long enough to matter: a sweep uploads up to ~16 MiB (both signals),
///   so 60 s is roughly 20 sweeps against a collector on a fast link -- the
///   whole 164 MB backlog above -- and proportionally fewer on a slow one,
///   which is exactly the trade a clock bound should make.
const BUDGET: Duration = Duration::from_secs(60);

/// Sweeps one wake may make. At 8 MiB a sweep that is 512 MiB, ~3x the largest
/// spool ever measured here. Deliberately not tight: [`BUDGET`] is the bound
/// that binds on any machine where a sweep is slow, and this one is what keeps
/// a wake finite where sweeps are fast enough that the clock never fires.
const MAX_PASSES: u32 = 64;

/// Drains until the spool is caught up, a sweep stops short, or a bound hits.
pub async fn once(
    http: &reqwest::Client,
    endpoint: &str,
    bearer: &Redacted<String>,
    spool_path: &Path,
    dry_run: bool,
) -> Result<()> {
    let state_dir = crate::cache::state_dir()?;
    // Held for the whole loop below, not just one sweep. See `lock`'s module
    // doc, and `BUDGET` for what that costs a concurrent hand-run.
    let _guard = lock::acquire(&state_dir)?;
    let wake = Wake {
        http,
        endpoint,
        bearer,
        spool: spool_path,
        checkpoint: checkpoint::path(&state_dir),
        dry_run,
    };

    let started = Instant::now();
    let mut tally = Tally::default();
    let failed = loop {
        let swept = sweep::once(&wake, tally.records).await?;
        tally.add(&swept);
        // ⚠️ `complete` first among equals: see the module doc. The other two
        // are "nothing left" and "the collector stopped taking things".
        if swept.failed.is_some() || !swept.complete || swept.pending == 0 {
            break swept.failed;
        }
        if tally.passes >= MAX_PASSES {
            tally.stopped_on = Some(format!("this wake's ceiling of {MAX_PASSES} sweeps"));
            break None;
        }
        if started.elapsed() >= BUDGET {
            tally.stopped_on = Some(format!("this wake's budget of {}s", BUDGET.as_secs()));
            break None;
        }
    };
    tally.report(spool_path);

    failed.map_or(Ok(()), Err)
}

/// The wake's running total, and what it says at the end.
#[derive(Default)]
struct Tally {
    passes: u32,
    /// Bytes the shared offset advanced over across every sweep.
    bytes: u64,
    records: u64,
    /// Bytes still undelivered as of the last sweep.
    pending: u64,
    /// Which bound ended the loop, if a bound did.
    stopped_on: Option<String>,
}

impl Tally {
    fn add(&mut self, swept: &Swept) {
        self.passes = self.passes.saturating_add(1);
        self.bytes = self.bytes.saturating_add(swept.advanced);
        self.records = self.records.saturating_add(swept.pushed);
        self.pending = swept.pending;
    }

    /// Silent for the ordinary one-sweep wake, which already reports itself:
    /// a backlogged machine has to *look* different in the journal from a
    /// healthy one, and it cannot do that if every wake prints the same line.
    fn report(&self, spool: &Path) {
        if self.passes <= 1 && self.stopped_on.is_none() {
            return;
        }
        let head = format!(
            "Drained {} sweeps of {} in one wake: {} byte(s), {} record(s).",
            self.passes,
            spool.display(),
            self.bytes,
            self.records
        );
        match (&self.stopped_on, self.pending) {
            (Some(bound), pending) => eprintln!(
                "{head} Stopped on {bound} with {pending} byte(s) still pending; every byte it \
                 did drain is checkpointed and the next wake continues from there."
            ),
            (None, 0) => eprintln!("{head} The spool is caught up."),
            (None, pending) => eprintln!(
                "{head} {pending} byte(s) are still pending because a sweep could not resolve \
                 everything it read -- the reason is above this line. Nothing was lost."
            ),
        }
    }
}
