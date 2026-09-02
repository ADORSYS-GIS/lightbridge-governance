//! Size-triggered **copy-truncate** rotation, and why it is not a rename.
//!
//! ## The bound
//!
//! One live file capped at [`MAX_BYTES`] plus [`KEEP`] rotated generations,
//! so the log directory holds at most `(KEEP + 1) * MAX_BYTES` = **4 MiB**,
//! for ever, on every machine. The check runs once per process at startup,
//! so the live file may exceed `MAX_BYTES` by whatever a single invocation
//! writes before the next one trims it -- kilobytes, and bounded by the run.
//! Without this, a binary invoked every 240s by Claude Code, again by Codex,
//! and again every 300s by the drain timer grows monotonically for ever;
//! the launchd agent's own `StandardErrorPath` capture already did, and the
//! plist template used to tell the reader to trim it by hand.
//!
//! ## Why copy-truncate, not rename
//!
//! The obvious rotation is `rename(log, log.1)` then create a fresh `log`.
//! It is wrong here, for a reason this file has already met once (see
//! `docs/governance-auth/files.md` on the Copilot spool): **this file has
//! writers we do not control**. launchd opens `StandardErrorPath` for the
//! job and holds it; a peer `governance-auth` process holds its own handle
//! from [`super::writer::open`]. `rename` moves the name, not the inode, so
//! every one of those handles keeps writing into `log.1` while the file
//! everyone reads stays empty -- a silent, total loss of the 03:00 record
//! this module exists to produce.
//!
//! Truncation keeps the inode, so every open handle stays attached to the
//! file that is still called `governance-auth.log`. It is safe against those
//! handles precisely because they are all `O_APPEND` (launchd's stdio
//! redirects and [`super::writer`]'s): an append re-resolves the offset at
//! write time, so a writer that was 900 KB in simply continues at 0 rather
//! than leaving a 900 KB hole. This is `logrotate(8)`'s `copytruncate`, for
//! the same reason `logrotate` offers it.
//!
//! The one thing copy-truncate cannot preserve is a write landing between
//! the copy and the truncate. That window is one `fs::copy` wide, it costs
//! at most a few lines, and no locking scheme removes it -- only asking
//! every writer to reopen would, and launchd offers no way to be asked.
//!
//! ## Concurrency
//!
//! Rotation itself is serialised with the same [`FileLock`] the session and
//! the drain use, so two processes cannot interleave the generation shift
//! (`.2` -> `.3`, `.1` -> `.2`) and leave a generation doubled or missing.
//! The lock is taken with a **zero** live-holder ceiling: a peer already
//! rotating means the work is being done, so this process skips it rather
//! than queueing behind it. Nothing here is fatal -- a rotation that cannot
//! run leaves an oversized log, which is worse than a trimmed one and much
//! better than a failed `token`.

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;

use crate::cache::{FileLock, state_dir};

/// Rotate once the live file passes this. 1 MiB is ~10k events, comfortably
/// more than any one debugging session needs to look back over.
pub(super) const MAX_BYTES: u64 = 1024 * 1024;

/// Rotated generations kept: `…log.1` (newest) through `…log.3`.
pub(super) const KEEP: usize = 3;

/// Best-effort: every failure path leaves the log as it was and returns.
pub(super) fn maybe_rotate(path: &Path) {
    if !oversized(path) {
        return;
    }
    let Ok(_lock) = lock() else {
        return;
    };
    // Re-checked under the lock: the peer we queued behind (however
    // briefly) may have just rotated this very file, and rotating again
    // would discard a generation for nothing.
    if oversized(path) {
        let _ = rotate(path, KEEP);
    }
}

fn oversized(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.len() > MAX_BYTES)
}

fn lock() -> Result<FileLock> {
    // Not per-issuer like the session lock: the log is one file for the
    // whole binary, so the lock guarding it is one file too.
    FileLock::acquire_at(state_dir()?.join("logs.lock"), Some(Duration::ZERO))
}

/// Shifts the generations down, copies the live file to `.1`, truncates it.
///
/// Split from [`maybe_rotate`] so the mechanism is reachable from a test
/// that neither takes a lock nor depends on a 1 MiB threshold.
pub(super) fn rotate(path: &Path, keep: usize) -> std::io::Result<()> {
    // Newest LAST: iterating in reverse is what stops `.1` overwriting `.2`
    // before `.2` has been moved out of the way. `.KEEP` is deliberately not
    // unlinked first -- `fs::rename` replaces an existing destination, so the
    // oldest generation falls off the end here on its own. Falsified: adding
    // the unlink back changed no test, removing the `.rev()` broke the bound.
    for index in (1..keep).rev() {
        let _ = fs::rename(generation(path, index), generation(path, index + 1));
    }
    if keep > 0 {
        fs::copy(path, generation(path, 1))?;
    }
    // `truncate`, NOT `remove_file` + recreate: see the module doc. The
    // inode has to survive, or every handle already open on it is orphaned.
    fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map(drop)
}

/// `governance-auth.log` -> `governance-auth.log.2`. Appended to the whole
/// file name rather than swapped into the extension, so the rotated files
/// sort next to the live one and nothing has to parse `.log` back out.
fn generation(path: &Path, index: usize) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}
