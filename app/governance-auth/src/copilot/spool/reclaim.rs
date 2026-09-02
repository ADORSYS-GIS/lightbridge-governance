//! Truncating the spool once every byte in it has been delivered.
//!
//! ## The claim this module overturned
//!
//! [`super`]'s module doc, [`crate::copilot`]'s, and
//! `docs/governance-auth/files.md` all said the same thing, and it was wrong:
//!
//! > truncating a file another process holds at offset N does not move that
//! > process's offset: the next append lands at N and the kernel zero-fills
//! > the gap
//!
//! That is true of a writer holding a plain `O_WRONLY` descriptor. It is
//! **false for this writer**, and the difference was measured on a live system
//! on 2026-09-02:
//!
//! 1. `lsof -o` on the live spool showed VS Code holding *three* write
//!    descriptors, all reporting an offset exactly equal to the file size and
//!    advancing in lockstep as the file grew. Three independent `open()` calls
//!    cannot stay synchronised unless every write seeks to EOF atomically --
//!    the signature of `O_APPEND`.
//! 2. The live spool was then truncated with VS Code running and holding those
//!    descriptors. Copilot's next append started at byte 0: `od -c` on the head
//!    showed `{ " h r T i m e " : [ …` with **no NUL hole**, and the first line
//!    parsed.
//! 3. `copilot-push` afterwards reported the file as shorter than the recorded
//!    offset, restarted at byte 0, delivered the new records, and left
//!    `discarded_total` at 0.
//!
//! So the spool is safe to truncate for exactly the reason
//! [`crate::logging::rotate`] is -- and note the symmetry: that module already
//! documented the `O_APPEND` argument in the affirmative, about the same OS
//! behaviour these docs asserted in the negative. One of the two was wrong,
//! and it was this one.
//!
//! ## Conservation, and the residual race
//!
//! The rule is that an offset advances only over records that were delivered
//! *or* recorded as lost. A truncate destroys bytes rather than advancing over
//! them, so it is only ever allowed to destroy bytes the offset has already
//! passed: the precondition is `size == checkpoint.offset` -- **exactly**, not
//! "at least" -- re-read from the open descriptor as late as the kernel
//! allows. Anything else, including one byte of a partial line, means bytes
//! exist that nobody has delivered, and the reclaim is skipped for that wake.
//!
//! That narrows the window. It does not close it, and no arrangement of
//! POSIX calls does: there is no atomic "truncate if the size is still N". An
//! append that lands after the `fstat` and before the `ftruncate` commits is
//! destroyed undelivered and uncounted. Measured here, 2,000 samples on APFS
//! against a 4 MiB file: `fstat` to `ftruncate` issue is 500 ns (p50), 4 µs
//! (p99), 22 µs (max); the `ftruncate` itself is 0.49 ms (p50), 0.53 ms (p99),
//! 0.95 ms (max), being dominated by freeing the extents. Take the whole
//! ≈0.5 ms as the window, since a `write` that takes the inode lock first
//! appends at the old EOF and is then discarded.
//!
//! What can be lost in it is one append -- one record, ~400 bytes at the
//! observed size. Against the heaviest rate ever measured on this spool
//! (12 MB in a few hours, ≈ one record every half second) an *unconditional*
//! 0.5 ms window would be hit about once in a thousand reclaims. The window is
//! not unconditional: `size == offset` says nothing was appended for the whole
//! preceding export pass -- an HTTP round trip, hundreds of milliseconds at
//! best -- or, on a wake that found nothing to drain, since the previous wake
//! up to five minutes ago. A writer silent for that long is not about to write
//! in the next half millisecond, so the realised rate is far under the
//! product. It is not zero, and nothing here claims it is.
//!
//! ## Why not the two ways of closing it
//!
//! **Rewrite the file to keep the undelivered tail.** That would let a reclaim
//! run without waiting to be caught up, and it is worse: the copy window is
//! milliseconds rather than microseconds, and it writes into a file another
//! process is appending to, so a write landing mid-rewrite interleaves rather
//! than being merely lost.
//!
//! **Punch a hole over `[0, offset)` and leave the file sparse.** This is
//! genuinely race-free -- it touches only bytes already delivered -- but it
//! needs `libc` and `unsafe` on two `#[cfg]` paths, it fails on filesystems
//! that do not support it, and it reclaims disk without bounding the file:
//! `ls -l` and `status` would both keep counting up for ever. Revisit it if
//! the residual race above ever produces an actual complaint.
//!
//! ## Order, and the crash window that is already handled
//!
//! Truncate first, write the checkpoint second. A crash in between leaves a
//! file shorter than the recorded offset, which is precisely
//! [`super::Restart::Truncated`] -- the drain restarts at byte 0 and says so.
//! The reverse order would leave an offset of 0 against a full file and
//! re-deliver everything.

use std::{fs::OpenOptions, path::Path};

use anyhow::{Context, Result};

use super::{Identity, identity};
use crate::copilot::checkpoint::{self, Checkpoint};

/// Leave a spool smaller than this alone.
///
/// 1 MiB, the same figure [`crate::logging::rotate::MAX_BYTES`] uses, and for
/// the same reason: it is far more than any wake needs and small enough that
/// the file is never a disk problem. A threshold rather than "reclaim whenever
/// caught up" because every reclaim is one exposure to the race above, and
/// there is nothing to gain from taking that exposure 288 times a day to
/// reclaim a few kilobytes.
///
/// The resulting bound is honest rather than hard: the spool holds at most
/// this plus whatever accrues between the wake that crosses it and the next
/// wake that finds itself fully caught up. A developer who never once presents
/// a caught-up wake is never reclaimed; at the observed rates a caught-up wake
/// arrives within minutes.
pub const RECLAIM_ABOVE: u64 = 1024 * 1024;

// ## How this reads against a backlog, and why that needed fixing
//
// `size == offset` is a precondition a backlogged machine could not meet.
// [`super`]'s `MAX_READ` capped a wake at 8 MiB, so a 164 MB spool (measured
// 2026-09-02) drained at 27 KB/s and stayed uncaught-up for ~18 wakes -- for
// ever if Copilot sustained more than that. The spools with the most to
// reclaim were the ones that could never present the precondition.
//
// [`crate::copilot::drain`] now sweeps repeatedly within one wake, each sweep
// still reading at most 8 MiB, and the sweep that finishes a backlog is the
// first one to hold `size == offset` -- `copilot_push_backlog.rs` measures
// 23.6 MB drained and reclaimed in a single wake. The loop is what makes this
// module reachable on the machines it was written for. Nothing below changed:
// the size is still re-read from the open descriptor per sweep, so a sweep
// that finished only part of a growing spool declines as it always did.

/// Reclaims if it is allowed to, and reports either way on stderr.
///
/// Nothing here is fatal. A spool that could not be reclaimed is a spool that
/// is too big, which is what the last release shipped; failing the wake over
/// it would turn a disk-hygiene problem into a delivery one.
pub fn best_effort(spool: &Path, checkpoint_path: &Path, state: &Checkpoint, dry_run: bool) {
    if dry_run {
        return;
    }
    match maybe(spool, checkpoint_path, state) {
        Ok(Some(bytes)) => eprintln!(
            "Reclaimed {bytes} byte(s) from {}: every byte in it had been delivered (size == \
             offset), so it was truncated and the checkpoint reset to byte 0. Copilot's \
             descriptors are O_APPEND, so its next record lands at byte 0.",
            spool.display()
        ),
        Ok(None) => {}
        Err(error) => eprintln!(
            "The spool at {} was not reclaimed: {error:#}. Nothing was consumed and the \
             checkpoint is unchanged; the next wake tries again.",
            spool.display()
        ),
    }
}

/// `Ok(Some(bytes))` when the spool was truncated, `Ok(None)` when it was not
/// eligible. Separate from [`best_effort`] so the precondition is reachable
/// from a test that does not have to read stderr.
pub fn maybe(spool: &Path, checkpoint_path: &Path, state: &Checkpoint) -> Result<Option<u64>> {
    let delivered = state.offset;
    if delivered <= RECLAIM_ABOVE {
        return Ok(None);
    }

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(spool)
        .with_context(|| format!("opening {} to reclaim it", spool.display()))?;
    // ⚠️ The precondition. Read from the open descriptor rather than the path,
    // and immediately before the truncate rather than reusing the size the
    // drain saw a whole export pass ago.
    let size = file
        .metadata()
        .with_context(|| format!("sizing {}", spool.display()))?
        .len();
    if size != delivered {
        return Ok(None);
    }
    file.set_len(0)
        .with_context(|| format!("truncating {}", spool.display()))?;

    let mut reclaimed = state.clone();
    reclaimed.restart();
    reclaimed.spool = current(spool)?;
    checkpoint::store(checkpoint_path, &reclaimed)?;
    Ok(Some(size))
}

/// The identity of the file as it stands after the truncate.
///
/// Usually empty, so the digest covers zero bytes and would match any file on
/// the same inode. That is safe here and only here: it travels with an offset
/// of 0, and a drain starting at 0 reads the whole file whether or not it
/// recognises it. A replacement is still caught by the inode. One wake later
/// the ordinary identity, over real bytes, has taken its place.
fn current(spool: &Path) -> Result<Option<Identity>> {
    match std::fs::metadata(spool) {
        Ok(metadata) => Ok(Some(identity::of(spool, &metadata)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("re-reading {}", spool.display())),
    }
}
