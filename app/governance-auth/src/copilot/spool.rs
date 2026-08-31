//! Reads the append-only spool from a byte offset, without ever writing to it.
//!
//! ## Why offset-only, and why the file is never truncated
//!
//! VS Code holds this file **open for append** for the life of the window. On
//! Linux, truncating a file another process holds at offset N does not move
//! that process's offset: the next append lands at N and the kernel zero-fills
//! the gap, so the spool grows a hole of NUL bytes and every subsequent parse
//! is garbage. On macOS the same is true. There is no portable way to make
//! "truncate underneath a live writer" safe, so this module does not try:
//! **the checkpoint is the only thing that advances.** Reclaiming disk is left
//! to the writer (Copilot rotates its own outfile) or to a human with VS Code
//! closed.
//!
//! ## Why only whole lines are consumed
//!
//! An append is not atomic with respect to a concurrent reader; a drain can
//! land mid-write and see half a JSON object. So the offset only ever advances
//! **past the last newline** actually read. A trailing partial line is left
//! where it is and picked up next run, which is also what makes re-running a
//! no-op when nothing new was written.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use anyhow::{Context, Result, bail};

/// The compiled-default spool name, under the state directory. Chosen so the
/// `outfile` a developer pastes into VS Code's settings and the path this
/// command reads with no configuration at all are the same string.
pub const DEFAULT_FILE_NAME: &str = "copilot-otel.jsonl";

/// Most bytes read in one drain. Bounds memory on a spool that grew while
/// nobody was draining it; the checkpoint advances by however much was
/// consumed, so the next run simply continues. 8 MiB is ~2,600 records at the
/// observed ~3 KiB/record.
const MAX_READ: u64 = 8 * 1024 * 1024;

pub struct Drain {
    /// Complete lines, in file order, with blank lines dropped.
    pub lines: Vec<String>,
    /// Where the next drain must start. Never past the last newline read.
    pub next_offset: u64,
    /// The file was shorter than the checkpoint: it was truncated or rotated
    /// under us and reading resumed from 0. Reported so a duplicated push is
    /// explicable rather than mysterious.
    pub restarted: bool,
    /// Size at the moment of the read, for `status`.
    pub size: u64,
}

/// Reads whole lines from `offset` onwards.
///
/// A missing spool is not an error: Copilot creates it on first export, and a
/// developer who has not used Chat yet must not see a failing timer.
pub fn drain(path: &Path, offset: u64) -> Result<Drain> {
    let size = match std::fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Drain {
                lines: Vec::new(),
                // The file this offset described is gone. Resuming at the old
                // offset against a *new* file would skip its first N bytes.
                next_offset: 0,
                restarted: offset > 0,
                size: 0,
            });
        }
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", path.display()));
        }
    };

    let (offset, restarted) = if size < offset {
        (0, true)
    } else {
        (offset, false)
    };

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
            size,
        });
    };

    let complete = buffer.get(..=last_newline).unwrap_or_default();
    let lines = complete
        .split(|byte| *byte == b'\n')
        .map(|line| String::from_utf8_lossy(line).trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect();

    Ok(Drain {
        lines,
        next_offset: offset
            .saturating_add(u64::try_from(last_newline).unwrap_or(u64::MAX))
            .saturating_add(1),
        restarted,
        size,
    })
}
