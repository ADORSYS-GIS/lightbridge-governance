//! Atomic, `fsync`-durable tmp-then-rename persistence for a small JSON state
//! file, generic over the value it holds.
//!
//! ## Why this exists as its own module
//!
//! [`crate::copilot::checkpoint`] wrote this exact pattern first: a reader
//! must never see a half-written value, so every store writes to `<path>.tmp`
//! and renames over the real path, and a missing file means "never written"
//! while an unreadable one is fatal (see this module's own doc below for why
//! defaulting on a parse error is unsafe either direction). `otel_daemon`'s
//! durable spool (#269) needs the identical guarantee for a differently-shaped
//! checkpoint -- the daemon is always-on and moves one record at a time, not a
//! periodic multi-signal wake, so its `Checkpoint` carries none of
//! `copilot::checkpoint::Checkpoint`'s `last_push_records` / `held_since_unix`
//! / per-signal-offset fields. Forcing the two shapes together would mean
//! every reader of one carries fields that mean nothing to it. So the shape
//! stays separate; only the write/read discipline is shared, here.
//!
//! `crate::copilot::checkpoint::{load, store}` are unchanged in behaviour --
//! they now call through to this module rather than duplicating its body.
//!
//! ## Why `fsync` reaches further than the house `write_private_file` pattern
//!
//! `cache.rs`/`otel.rs`/`update.rs` each `fsync` the tmp file's contents
//! before renaming, which is enough for them: an interrupted rename there
//! leaves the *previous* value in place, and the next read either sees the
//! old value or (once the rename lands) the new one -- never a truncated
//! file, because the rename itself is atomic at the filesystem's directory
//! layer regardless of whether that directory entry update has itself been
//! flushed. What is NOT guaranteed without also `fsync`ing the parent
//! directory is that the rename *survives* a crash at all: on a crash before
//! the directory's own metadata reaches disk, the rename can be entirely
//! rolled back on reboot, silently reverting to the old file -- ordinarily
//! harmless (this module's own callers are outage-tolerant), except that
//! `load`'s contract below is fatal on anything it cannot parse, on both
//! callers ([`crate::copilot::checkpoint`] and `otel_daemon::checkpoint`).
//! Losing a durable rename is not what bricks the daemon -- a *torn* one is
//! (an old file rolled back is still valid, complete JSON) -- but the two
//! failure modes share one root cause (an unsynced tmp file) closely enough,
//! and the fix (`fsync` the tmp file, then `fsync` the directory once the
//! rename lands) is cheap enough on a write this small and this infrequent,
//! that both are closed together rather than reasoning about which specific
//! crash window each one alone would need.

use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::{Serialize, de::DeserializeOwned};

use crate::copilot::private_file;

/// A missing file means "never written", which is the honest starting state
/// for a checkpoint. An unreadable one is an `Err` -- see the module doc on
/// why neither `T::default()` nor any other silent fallback is safe here.
pub fn load<T: Default + DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "parsing the state file at {}. Delete it to restart from the beginning (which may \
             re-send or re-derive records already delivered) -- deleting it is a decision for \
             whoever is looking, not for this process.",
            path.display()
        )
    })
}

/// Writes tmp-then-rename so a reader never sees a half-written value, `fsync`s
/// the tmp file before the rename and the parent directory after it (see the
/// module doc's ⚠️), so neither a torn write nor a lost rename survives a
/// crash to be handed to [`load`]'s fatal-on-unparseable contract.
pub fn store<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let dir = path
        .parent()
        .context("the state path has no parent directory")?;
    private_file::create_dir(dir)?;

    let bytes = serde_json::to_vec(value).context("serialising state")?;
    let tmp = path.with_extension("json.tmp");
    write_durably(&tmp, &bytes)?;
    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    sync_dir(dir).with_context(|| format!("syncing {} after renaming into it", dir.display()))
}

#[cfg(unix)]
fn write_durably(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("opening {} for writing", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("writing {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing {} to disk", path.display()))
}

#[cfg(not(unix))]
fn write_durably(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

/// `fsync` on the directory descriptor itself -- the only way to make a
/// rename's directory-entry update durable, distinct from `fsync`ing either
/// file involved. Windows has no equivalent (`File::open` on a directory
/// fails there), and NTFS's own metadata journal already makes a rename
/// crash-safe without this step, so the no-op is correct, not a gap.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<()> {
    fs::File::open(dir)?.sync_all().map_err(Into::into)
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests;
