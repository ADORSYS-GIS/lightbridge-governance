//! Where governance-auth puts its files.
//!
//! One convention per KIND of data, not one per platform: state under
//! `$XDG_STATE_HOME` (Application Support on macOS), config under `~/.config`
//! (see `crate::otel`), and the legacy cache location stays with the session
//! store that migrated off it (`crate::cache::cache_dir`).
//!
//! Split out of `crate::cache` because the session file and the advisory
//! [`crate::file_lock::FileLock`] both live beside these paths, and a lock
//! needing the session module's private helpers was the wrong dependency
//! direction -- see the split in ADORSYS-GIS/lightbridge-governance#235.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result};

/// `$XDG_STATE_HOME` (or `~/.local/state`) on Linux,
/// `~/Library/Application Support` on macOS.
///
/// macOS deliberately does NOT get `~/.local/state`: the entire reason for
/// moving off `~/Library/Caches` is that the OS may purge it, and Apple's
/// non-purgeable per-user location is Application Support. (Config stays at
/// `~/.config` on both platforms -- see `crate::otel`, which already writes
/// there on macOS. One convention per KIND of data, not one per platform.)
pub(crate) fn state_dir() -> Result<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join("governance-auth"));
    }

    let home = std::env::var("HOME")
        .context("locating the state directory ($XDG_STATE_HOME and $HOME both unset)")?;
    let home = PathBuf::from(home);

    let base = if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support")
    } else {
        home.join(".local").join("state")
    };
    Ok(base.join("governance-auth"))
}

/// Creates the state directory at `0700`.
///
/// The files inside are already `0600`, so this is defence in depth -- but
/// it costs one line and it stops the DIRECTORY LISTING (which leaks the
/// set of issuer/client pairs this developer has sessions for) being
/// world-readable. `create_dir_all` alone applies the umask, which on a
/// typical laptop yields `0755`.
#[cfg(unix)]
pub(crate) fn create_state_dir(dir: &std::path::Path) -> Result<()> {
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
pub(crate) fn create_state_dir(dir: &std::path::Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating state directory {}", dir.display()))
}
