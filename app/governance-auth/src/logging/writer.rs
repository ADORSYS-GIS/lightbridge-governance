//! The sink half of [`super`]: opening the file, and handing `tracing` a
//! writer over it.
//!
//! One `O_APPEND` handle for the life of the process, shared by every event.
//! `O_APPEND` is the whole concurrency story: the kernel resolves the offset
//! at write time, so the drain timer and both editors' credential helpers can
//! hold this file open at once without overwriting each other, and a
//! truncation by whichever of them rotates (see [`super::rotate`]) does not
//! strand the others at a stale offset.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::Arc,
};

use anyhow::{Context, Result};
use tracing_subscriber::fmt::MakeWriter;

/// Opens `path` for append, creating it and its parent, and rotating first
/// if the file is already over the bound -- so this process appends to a
/// file that has been brought back under the bound rather than to the tail
/// of an oversized one.
pub(super) fn open(path: &Path) -> Result<LogFile> {
    let dir = path.parent().context("log path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    super::rotate::maybe_rotate(path);
    let file = options()
        .append(true)
        .create(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    Ok(LogFile(Arc::new(file)))
}

/// `0600` -- the file holds no secret by design, but it does hold this
/// developer's issuer, endpoints and failure history, and `~/Library/Logs`
/// is not itself a private directory.
#[cfg(unix)]
fn options() -> OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = OpenOptions::new();
    options.mode(0o600);
    options
}

#[cfg(not(unix))]
fn options() -> OpenOptions {
    OpenOptions::new()
}

#[derive(Clone)]
pub(super) struct LogFile(Arc<File>);

impl<'a> MakeWriter<'a> for LogFile {
    type Writer = Appender;

    fn make_writer(&'a self) -> Self::Writer {
        Appender(Arc::clone(&self.0))
    }
}

/// `tracing_subscriber`'s formatter renders a whole event into a buffer and
/// hands it over in one call, so one event is one `write` on an `O_APPEND`
/// file -- which is why concurrent processes never interleave half-lines.
/// No mutex: the ordering guarantee is the kernel's, not ours.
pub(super) struct Appender(Arc<File>);

impl Write for Appender {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self.0).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&*self.0).flush()
    }
}
