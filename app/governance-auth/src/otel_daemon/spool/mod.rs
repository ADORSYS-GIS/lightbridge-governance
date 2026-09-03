//! The durable spool: retains payloads on a refused mint or an unreachable
//! collector, on disk, so a killed daemon loses nothing it had already
//! accepted from a client (#269, AC1/AC2).
//!
//! ## What is reused from `copilot`, and why it is safe to
//!
//! The tailer ([`crate::copilot::spool::drain`]), the file-identity/rotation
//! check it uses ([`crate::copilot::spool::Identity`]), the 0600 write
//! discipline ([`crate::copilot::private_file`]) and the
//! two-separate-refusals quarantine table
//! ([`crate::copilot::quarantine::Quarantine`]) are all content-agnostic --
//! none of them parses a record, they only account for bytes and file
//! identity. That is what makes them safe to point at a completely different
//! file than the one they were written for: this spool is written *and* read
//! entirely by this daemon, never by an external process the way Copilot's
//! outfile is, so "rotation" here should in practice only ever be this
//! module's own reclaim ([`commit`]) -- the identity check runs anyway, at
//! zero extra cost, as the same defence in depth it is for Copilot.
//!
//! What is **not** reused is `copilot::checkpoint`/`copilot::journal`: see
//! [`super::checkpoint`]'s module doc for why the shapes diverge enough that
//! forcing them together would be worse than the small amount of duplication
//! here.
//!
//! ## The envelope, and why base64
//!
//! Each retained payload is one JSON line: `{"signal":"metrics","body":
//! "<base64>"}` (see [`envelope`]). The payload can be OTLP protobuf
//! ([`super::classify`] routes on the body, but does not require it to be
//! JSON), so it cannot be written as a raw line -- a protobuf byte can *be*
//! `\n`, which would silently fracture [`crate::copilot::spool::drain`]'s
//! line splitting. Base64 costs ~33% on disk; the alternative
//! (a length-prefixed binary frame) would need its own reader instead of
//! reusing the text tailer, for a spool whose whole purpose is to be a
//! short-lived bridge across an outage, not a compact long-term store.
//! Simplicity and reuse win here.
//!
//! ## The one thing this format is not proof against
//!
//! A write that is torn by a kill mid-`write` -- rare on a local filesystem
//! for the few-hundred-byte lines here, and the same residual risk
//! `copilot::spool`'s own module doc accepts for VS Code's writer -- can leave
//! a newline-less fragment at EOF. The next append lands after it (this
//! writer is `O_APPEND`, like Copilot's), fusing the fragment onto the front
//! of what should have been a clean, separate line. [`read::next`] treats an
//! envelope that fails to parse the same way [`super::classify`] treats a
//! signal it cannot read: log it, count it in `discarded_total`, and move
//! past it -- a bounded, honestly-counted loss of at most the one record
//! adjacent to the kill, not a stall and not a silent one. AC1/AC2's "loses
//! zero records" is about an ordinary kill between complete operations, which
//! is what `tests/serve_otel_durability.rs` exercises; this paragraph is the
//! named exception, not a contradiction of it.

mod commit;
mod envelope;
mod read;
#[cfg(test)]
mod tests;

use std::path::PathBuf;

use anyhow::{Context, Result};

use super::checkpoint::{self, Checkpoint};
use crate::copilot::{Signal, spool::Identity};

/// The file name under the state directory. No CLI override exists for it
/// (unlike Copilot's spool path): that override exists so a developer's
/// `--copilot-spool-path` can match VS Code's own `outfile` setting, and
/// nothing outside this daemon ever names this file.
pub const FILE_NAME: &str = "otel-daemon-spool.jsonl";

/// Total unconsumed bytes (file size minus the checkpoint offset) the spool
/// will retain. 16 MiB, unchanged from #268's in-memory cap: twice
/// [`crate::copilot::spool::MAX_READ`], room for a burst without unbounded
/// growth. At capacity, [`DurableSpool::retain`] refuses rather than
/// dropping the oldest -- the unavailable branch is the restrictive branch.
///
/// `usize` (not `u64`) so [`super::receive::MAX_BODY_SIZE`] can keep
/// mirroring it directly, matching axum's own `to_bytes` limit type.
pub const CAPACITY: usize = 16 * 1024 * 1024;

/// One durably-retained record, ready to be re-offered to the collector.
pub struct Pending {
    pub signal: Signal,
    pub payload: Vec<u8>,
    /// A stable, content-derived name for this line, for
    /// [`crate::copilot::quarantine::Quarantine`].
    key: String,
    /// Where the *next* record starts -- not the tail read's own
    /// `next_offset`, which spans every complete line that read happened to
    /// return. See [`read`].
    boundary: u64,
}

pub struct DurableSpool {
    spool_path: PathBuf,
    checkpoint_path: PathBuf,
    checkpoint: Checkpoint,
    /// The spool file's identity as of the last successful tail, adopted
    /// into the checkpoint only when something durable actually happens --
    /// mirrors `copilot::journal::Journal`'s same deferral, for the same
    /// reason: adopting it must never itself be a reason to write.
    pending_identity: Option<Identity>,
}

impl DurableSpool {
    /// Loads (or starts) the checkpoint at the compiled-default state-dir
    /// location. Never fails on a missing spool or checkpoint -- only an
    /// unreadable one, which is fatal by design (see
    /// [`crate::durable_state`]).
    pub fn open() -> Result<Self> {
        let dir = crate::cache::state_dir()?;
        Self::at(dir.join(FILE_NAME), checkpoint::path(&dir))
    }

    fn at(spool_path: PathBuf, checkpoint_path: PathBuf) -> Result<Self> {
        let checkpoint = checkpoint::load(&checkpoint_path)?;
        Ok(Self {
            spool_path,
            checkpoint_path,
            checkpoint,
            pending_identity: None,
        })
    }

    /// Durably retains `payload` for a later retry, refusing (not silently
    /// dropping) once [`CAPACITY`] worth of unconsumed bytes is already on
    /// disk.
    pub fn retain(&mut self, signal: Signal, payload: Vec<u8>) -> Result<()> {
        let line = envelope::encode(signal, &payload)?;

        let size = match std::fs::metadata(&self.spool_path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => {
                return Err(error).with_context(|| format!("sizing {}", self.spool_path.display()));
            }
        };
        let pending = size.saturating_sub(self.checkpoint.offset);
        let added = u64::try_from(line.len().saturating_add(1)).unwrap_or(u64::MAX);
        let capacity = u64::try_from(CAPACITY).unwrap_or(u64::MAX);
        if pending.saturating_add(added) > capacity {
            anyhow::bail!(
                "daemon spool full ({pending} of {CAPACITY} bytes retained); the collector may \
                 be unreachable -- retry later"
            );
        }
        envelope::append_line(&self.spool_path, &line)
    }
}
