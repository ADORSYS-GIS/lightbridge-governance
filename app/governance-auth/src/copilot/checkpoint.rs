//! Where the drain got to: `<state_dir>/copilot-push.json`, mode `0600`.
//!
//! State, not cache, for the same reason the session is (see
//! [`crate::cache`]'s module doc): losing this file does not log anyone out,
//! but it does mean the next run re-pushes the whole spool, which is
//! duplicate billing data at the collector. macOS purging
//! `~/Library/Caches` must not be able to cause that.
//!
//! ## Why an unparseable checkpoint is fatal
//!
//! Defaulting to offset 0 on a file we cannot read would re-push everything
//! already sent, silently. Defaulting to the file's current size would
//! silently *discard* everything not yet sent. Both are wrong in a way nobody
//! would notice, so this bails and names the file: deleting it is a decision
//! for whoever is looking, not for this process.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The file name under the state dir. Sibling of the session files, which are
/// named by `sha256(issuer + client_id)` -- this one is not, deliberately:
/// the spool is a property of the *machine's* VS Code install, not of which
/// identity happens to be pushing it, and two checkpoints for one spool would
/// each skip the other's bytes.
pub const FILE_NAME: &str = "copilot-push.json";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Bytes of the spool already transformed and accepted by the collector.
    #[serde(default)]
    pub offset: u64,
    /// Unix seconds of the last push the collector accepted. `None` until one
    /// succeeds -- `status` shows "never", which is the point: a timer that
    /// has been failing since it was installed must not look like a fresh one.
    #[serde(default)]
    pub last_push_unix: Option<u64>,
    /// Records in that push. Zero is legitimate (a run with nothing new).
    #[serde(default)]
    pub last_push_records: u64,
}

pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

/// A missing checkpoint means "never drained", which is the honest starting
/// state. An unreadable one is an error -- see the module doc.
pub fn load(path: &Path) -> Result<Checkpoint> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Checkpoint::default());
        }
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "parsing the Copilot push checkpoint at {}. Delete it to restart the drain from the \
             beginning of the spool (which re-sends records the collector may already have).",
            path.display()
        )
    })
}

/// Writes tmp-then-rename so a reader never sees a half-written offset, and
/// an interrupted write leaves the previous checkpoint intact rather than a
/// truncated one this module would then refuse to parse.
pub fn store(path: &Path, checkpoint: &Checkpoint) -> Result<()> {
    let dir = path
        .parent()
        .context("the checkpoint path has no parent directory")?;
    create_private_dir(dir)?;

    let bytes = serde_json::to_vec(checkpoint).context("serialising the push checkpoint")?;
    let tmp = path.with_extension("json.tmp");
    write_private(&tmp, &bytes)?;
    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))
}

pub fn now_unix() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("reading the system clock")?
        .as_secs())
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    if dir.is_dir() {
        return Ok(());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating state directory {}", dir.display()))
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating state directory {}", dir.display()))
}

/// `0600` even though an offset is not secret: the state directory holds
/// session files and this one sits beside them, so it inherits their
/// permissions rather than introducing the one world-readable file in it.
#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("opening {} for writing", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}
