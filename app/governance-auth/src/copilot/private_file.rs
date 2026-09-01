//! `0700` directories and `0600` files, for the drain's own state.
//!
//! Split out of [`super::checkpoint`] to keep that module under the 200-LoC
//! gate once the quarantine table joined it. An offset is not secret, but the
//! state directory holds session files and these sit beside them -- inheriting
//! their permissions rather than introducing the one world-readable file in it
//! is the whole reason these are not `fs::write`.

use std::{fs, path::Path};

use anyhow::{Context, Result};

#[cfg(unix)]
pub fn create_dir(dir: &Path) -> Result<()> {
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
pub fn create_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating state directory {}", dir.display()))
}

#[cfg(unix)]
pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
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
pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}
