//! Private, crash-safe writes for generated client configuration.

use std::{fs, path::Path};

use anyhow::{Context, Result};

/// tmp-then-rename at mode 0600. Claude Code's and Codex's files can carry the
/// OTLP bearer token, so they get the same treatment as the session cache --
/// and an interrupted write must never leave a half-file behind, since Codex
/// refuses to start on a malformed config rather than degrading. VS Code's
/// (`crate::vscode`) carries no credential since the file-exporter cutover and
/// is written through here anyway: one writer, one set of guarantees.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("governance-auth-tmp");
    write_private_file(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes)?;
    Ok(())
}
