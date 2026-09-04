//! `0700` directories, for the drain's own state.
//!
//! Split out of [`super::checkpoint`] to keep that module under the 200-LoC
//! gate once the quarantine table joined it. An offset is not secret, but the
//! state directory holds session files and these sit beside them -- inheriting
//! their permissions rather than introducing the one world-readable directory
//! entry in it is the whole reason this is not `fs::create_dir_all`.
//!
//! The 0600-file half of this module moved to [`crate::durable_state`] (#269/
//! #291 review, P1-2): every caller of the old `write` here wanted
//! tmp-then-rename durability anyway, and that module is now the one place
//! deciding how a state file is written, `fsync` included.

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
