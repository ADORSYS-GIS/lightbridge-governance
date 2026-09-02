//! Which file an offset was measured against.
//!
//! ## Why size cannot answer this
//!
//! `size < offset` was the only rotation the drain recognised, and it answers
//! the wrong question. VS Code recreates its outfile on restart; the developer
//! keeps working; by the time the five-minute timer next fires the **new** file
//! is already longer than the offset the **old** one left behind. The
//! comparison is then false, the drain seeks into the middle of a file it has
//! never read, and every record before that byte is skipped -- not delivered,
//! not counted, offset at the end. Measured: a 2,700-byte spool replaced by a
//! 5,412-byte one lost six brand-new records with `discarded_total` moving by
//! one, for the partial-line fragment at the resume point.
//!
//! That is not a corner case. The spool was measured growing 73 KB -> 315 KB in
//! six minutes of ordinary use, so outgrowing a stale offset inside one timer
//! window is the *ordinary* outcome of a VS Code restart. Pointing
//! `--copilot-spool-path` at a different, larger file reaches it too.
//!
//! ## Why both an inode and a digest, when either sounds sufficient
//!
//! Neither is. **inode+device** identifies a file exactly -- until the kernel
//! reuses an inode number, at which point a brand-new file inherits the old
//! one's identity and the skip happens anyway. **A digest of the head** is
//! immune to that, because the leading bytes of an append-only file never
//! change, so a digest over them is a stable name for the same file; but it
//! cannot tell one file from a byte-identical copy, which is exactly what a
//! copy-truncate rotation produces. So both must agree, and a disagreement in
//! either is answered by starting over. ADR-0012 puts this binary on Linux and
//! macOS, where `MetadataExt` supplies the first half on both.
//!
//! A spurious restart costs a re-export -- duplicates, which this drain works
//! hard to avoid -- so the conditions are deliberately narrow: an id that is
//! *unknown* on either side is not a mismatch, only two known ids that differ.

use std::{fs::File, io::Read, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How much of the head the digest covers. 4 KiB is one page and roughly the
/// first record and a half of a real spool; the cost is one short read per
/// wake, against a five-minute timer.
const HEAD_BYTES: u64 = 4096;

/// A name for the file an offset belongs to, stable under appends.
///
/// The fields are private: every comparison has to go through [`same`], which
/// is where the "unknown is not a mismatch" rule lives. A caller reaching in to
/// compare two inode numbers directly would silently skip it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// `None` off Unix, and on a checkpoint written by a build that had no
    /// `MetadataExt` to ask.
    #[serde(default)]
    inode: Option<u64>,
    #[serde(default)]
    device: Option<u64>,
    /// Hex SHA-256 of the first `head_len` bytes. Not the bytes themselves:
    /// `AGENTS.md` bans writing a payload anywhere, and a spool line is
    /// prompt-adjacent telemetry.
    head: String,
    /// How many bytes `head` actually covers. Recorded rather than assumed,
    /// because a spool shorter than [`HEAD_BYTES`] digests all of itself --
    /// and comparing that against a digest of 4 KiB of the same, longer, file
    /// would report every growing spool as a different one.
    head_len: u64,
}

/// Why a drain must go back to byte 0. Two causes, and neither subsumes the
/// other: a copy-truncate rotation keeps the inode and resets the size, while
/// a replace-on-restart changes the file and the size only grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restart {
    /// The file is shorter than the checkpoint.
    Truncated,
    /// The file is not the one the offset was measured against.
    Replaced,
}

impl Restart {
    /// What to tell whoever is reading stderr. The two read differently on
    /// purpose: one sends the reader looking for a truncation and the other
    /// for a file swap, and being sent to the wrong one is worse than being
    /// told nothing.
    pub fn explain(self, path: &Path, offset: u64) -> String {
        match self {
            Self::Truncated => format!(
                "The spool at {} is shorter than the recorded offset ({offset} bytes): it was \
                 truncated or rotated, so the drain restarted at byte 0.",
                path.display()
            ),
            Self::Replaced => format!(
                "The spool at {} is not the file byte {offset} was measured against -- it was \
                 replaced, not appended to (VS Code recreating its outfile does this). The drain \
                 restarted at byte 0, so records the old file already delivered may arrive again \
                 if the two files share content.",
                path.display()
            ),
        }
    }
}

/// The identity of the file at `path` as it is right now.
pub fn of(path: &Path, metadata: &std::fs::Metadata) -> Result<Identity> {
    read(path, metadata, HEAD_BYTES.min(metadata.len()))
}

/// Whether the drain must start over, and why.
///
/// Size is asked first because it is the cheap, unambiguous signal -- and
/// because reporting a truncation as a replacement sends whoever reads the
/// message looking for a file swap that never happened.
pub fn restart(
    known: Option<&Identity>,
    path: &Path,
    metadata: &std::fs::Metadata,
    offset: u64,
) -> Result<Option<Restart>> {
    if metadata.len() < offset {
        return Ok(Some(Restart::Truncated));
    }
    match known {
        Some(recorded) if !same(recorded, path, metadata)? => Ok(Some(Restart::Replaced)),
        _ => Ok(None),
    }
}

/// Whether `recorded` names the file now at `path`.
///
/// The digest is recomputed over exactly `recorded.head_len` bytes, never over
/// the current head length, so a file that has merely grown still matches.
fn same(recorded: &Identity, path: &Path, metadata: &std::fs::Metadata) -> Result<bool> {
    // Shorter than the prefix the digest covers: whatever this file is, it is
    // not the recorded one grown by appends. (`size < offset` catches most of
    // these first; this catches a truncation to *between* the digest window
    // and the offset.)
    if metadata.len() < recorded.head_len {
        return Ok(false);
    }
    let current = read(path, metadata, recorded.head_len)?;
    Ok(agree(recorded.inode, current.inode)
        && agree(recorded.device, current.device)
        // A short read means the file shrank between the stat and the read.
        // Different lengths are not comparable, so they are not a match.
        && current.head_len == recorded.head_len
        && current.head == recorded.head)
}

/// Two ids match unless both are known and they differ. An id this platform or
/// an older build could not supply says nothing, and "says nothing" must not
/// read as "different" -- that would restart the drain on every wake.
fn agree(recorded: Option<u64>, current: Option<u64>) -> bool {
    match (recorded, current) {
        (Some(before), Some(now)) => before == now,
        _ => true,
    }
}

fn read(path: &Path, metadata: &std::fs::Metadata, head_len: u64) -> Result<Identity> {
    let file = File::open(path).with_context(|| format!("opening the spool {}", path.display()))?;
    let mut head = Vec::new();
    file.take(head_len)
        .read_to_end(&mut head)
        .with_context(|| format!("reading the head of {}", path.display()))?;
    let (inode, device) = ids(metadata);
    Ok(Identity {
        inode,
        device,
        head: hex::encode(Sha256::digest(&head)),
        head_len: u64::try_from(head.len()).unwrap_or(u64::MAX),
    })
}

/// `(inode, device)`, or `(None, None)` where the platform has no such thing.
///
/// One function returning a pair rather than two returning `Option<u64>`: on
/// Unix each of those would always be `Some`, which is `clippy::
/// unnecessary_wraps`, and the `Option` is not redundant -- it is the whole
/// cross-platform contract, and [`agree`] is built on it.
fn ids(metadata: &std::fs::Metadata) -> (Option<u64>, Option<u64>) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        (Some(metadata.ino()), Some(metadata.dev()))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        (None, None)
    }
}
