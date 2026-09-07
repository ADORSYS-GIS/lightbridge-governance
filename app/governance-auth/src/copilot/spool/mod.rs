//! Reads the append-only spool from a byte offset.
//!
//! ## Why reading is offset-only, and where the truncation went
//!
//! Nothing on the read path writes to the spool: a reader that rewrote the
//! file it is reading would have to interleave with VS Code's appends.
//! Reclaiming the file is a separate, deliberate act with a precondition of
//! its own, and it lives in [`reclaim`] -- which also carries the correction
//! to what this paragraph used to say. It claimed that truncating under a live
//! writer leaves the next append at the old offset with the gap zero-filled,
//! and that there was therefore no safe truncation to implement. Measured
//! false on 2026-09-02: Copilot's descriptors are `O_APPEND`, so the next
//! append lands at byte 0. See [`reclaim`] for the measurement.
//!
//! ## Why only whole lines are consumed
//!
//! An append is not atomic with respect to a concurrent reader; a drain can
//! land mid-write and see half a JSON object. So the offset only ever advances
//! **past the last newline** actually read. A trailing partial line is left
//! where it is and picked up next run, which is also what makes re-running a
//! no-op when nothing new was written.
//!
//! ## Why an offset alone is not enough to resume
//!
//! An offset is a byte count into *some* file, and nothing said which until an
//! [`Identity`] travelled beside it -- see [`identity`] for the six records
//! that cost.

mod identity;
pub mod reclaim;

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use anyhow::{Context, Result, bail};
pub use identity::{Identity, Restart};

/// The compiled-default spool name, under the state directory. Chosen so the
/// `outfile` a developer pastes into VS Code's settings and the path this
/// command reads with no configuration at all are the same string.
pub const DEFAULT_FILE_NAME: &str = "copilot-otel.jsonl";

/// Most bytes read in one call. Bounds memory on a spool that grew while
/// nobody was draining it; the checkpoint advances by however much was
/// consumed, so the next call simply continues. 8 MiB is ~2,600 records at the
/// observed ~3 KiB/record.
///
/// ⚠️ This is a **memory** bound and nothing else. It used to be a throughput
/// bound as well, because one wake made exactly one of these calls -- measured
/// at 8,385,060 bytes per wake against a 164 MB spool, so 27 KB/s at the
/// five-minute interval and ~18 wakes to catch up. [`crate::copilot::drain`]
/// now repeats the call within one wake instead, which is why raising this
/// number is not the answer to a backlog: it would trade memory for throughput
/// and still leave a fixed ceiling per wake.
///
/// `pub(crate)`: `otel_daemon::spool` (#269) reuses [`drain`], and its own
/// record-size cap must stay strictly under this one -- a single encoded
/// line at or above `MAX_READ` makes `drain` bail permanently (the `bail!`
/// below) -- so it references the real constant, not a guessed copy.
pub(crate) const MAX_READ: u64 = 8 * 1024 * 1024;

/// One complete record and where it starts in the file. The offset is what
/// lets a signal that was already accepted skip the lines it delivered
/// (see [`super::checkpoint`]'s per-signal offsets) without re-deriving them
/// from a second parse.
#[derive(Debug)]
pub struct Line {
    pub offset: u64,
    pub text: String,
}

impl AsRef<str> for Line {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

pub struct Drain {
    /// Complete lines, in file order, with blank lines dropped.
    pub lines: Vec<Line>,
    /// Where the next drain must start. Never past the last newline read.
    pub next_offset: u64,
    /// The file was truncated or replaced under us and reading resumed from 0.
    /// Reported so a duplicated push is explicable rather than mysterious.
    pub restarted: Option<Restart>,
    /// The file is not there at all. Distinct from `restarted`, and the
    /// distinction is not cosmetic -- see [`drain`].
    pub missing: bool,
    /// Size at the moment of the read, for `status`.
    pub size: u64,
    /// What the file was, so the checkpoint can record which file its offset
    /// belongs to. `None` only when there is no file.
    pub identity: Option<Identity>,
}

/// Reads whole lines from `offset` onwards.
///
/// A missing spool is not an error: Copilot creates it on first export, and a
/// developer who has not used Chat yet must not see a failing timer.
///
/// ⚠️ It is also **not a rotation**, and must not rewind the offset. A path
/// that does not exist says nothing about how far the real spool was drained
/// -- and the reasons to be pointed at one are mundane (a typo'd
/// `--copilot-spool-path`, an edited config, a home directory not mounted yet,
/// a run before VS Code recreated the file). Returning `next_offset: 0` here
/// let one such run reset the checkpoint, after which the next *correct* run
/// re-exported the entire spool. So the offset comes back untouched and
/// `missing` tells the caller to do nothing at all.
///
/// `known` is the identity recorded for the file `offset` was measured
/// against. `None` -- a checkpoint written before identities existed -- says
/// *nothing* about the file, so it is adopted rather than read as a mismatch:
/// reading it as one would re-export every developer's whole spool on upgrade.
pub fn drain(path: &Path, offset: u64, known: Option<&Identity>) -> Result<Drain> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Drain {
                lines: Vec::new(),
                next_offset: offset,
                restarted: None,
                missing: true,
                size: 0,
                identity: None,
            });
        }
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", path.display()));
        }
    };
    let size = metadata.len();
    let identity = Some(identity::of(path, &metadata)?);
    let restarted = identity::restart(known, path, &metadata, offset)?;
    let offset = if restarted.is_some() { 0 } else { offset };

    let mut file =
        File::open(path).with_context(|| format!("opening the spool {}", path.display()))?;
    file.seek(SeekFrom::Start(offset))
        .with_context(|| format!("seeking to byte {offset} in {}", path.display()))?;

    let mut buffer = Vec::new();
    file.take(MAX_READ)
        .read_to_end(&mut buffer)
        .with_context(|| format!("reading {}", path.display()))?;

    let Some(last_newline) = buffer.iter().rposition(|byte| *byte == b'\n') else {
        // A whole capped read with no newline means one record is larger than
        // the cap; advancing anyway would corrupt it and not advancing loops
        // forever. Neither is acceptable silently, so say so and stop.
        if u64::try_from(buffer.len()).unwrap_or(u64::MAX) >= MAX_READ {
            bail!(
                "{} has a single record larger than the {MAX_READ}-byte read limit at byte \
                 {offset}; the drain cannot advance past it. Close VS Code and remove the file, \
                 or point --copilot-spool-path at a fresh one.",
                path.display()
            );
        }
        // Otherwise: only a partial line so far. Normal, and a no-op.
        return Ok(Drain {
            lines: Vec::new(),
            next_offset: offset,
            restarted,
            missing: false,
            size,
            identity,
        });
    };

    let complete = buffer.get(..=last_newline).unwrap_or_default();
    let mut lines = Vec::new();
    let mut position = offset;
    for segment in complete.split(|byte| *byte == b'\n') {
        let start = position;
        position = position
            .saturating_add(u64::try_from(segment.len()).unwrap_or(u64::MAX))
            .saturating_add(1);
        let text = String::from_utf8_lossy(segment).trim().to_owned();
        if text.is_empty() {
            continue;
        }
        lines.push(Line {
            offset: start,
            text,
        });
    }

    Ok(Drain {
        lines,
        next_offset: offset
            .saturating_add(u64::try_from(last_newline).unwrap_or(u64::MAX))
            .saturating_add(1),
        restarted,
        missing: false,
        size,
        identity,
    })
}
