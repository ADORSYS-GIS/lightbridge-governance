//! Marker-delimited block editing for shell rc files. Everything between the
//! markers is replaced wholesale on each run; everything outside is never
//! touched. Without markers the only idempotent options are "append every
//! time" (the block accumulates forever) or "rewrite the file" (destroys the
//! developer's own config).

use std::{fs, path::Path};

use anyhow::{Context, Result};

/// Marker pair delimiting the block this binary owns in a shell rc file.
pub(crate) const BLOCK_BEGIN: &str = "# >>> governance-auth otel (managed) >>>";
pub(crate) const BLOCK_END: &str = "# <<< governance-auth otel (managed) <<<";

/// Replaces the managed block in `path`, or appends one if absent. Everything
/// outside the markers is preserved byte-for-byte.
pub(crate) fn upsert_block(path: &Path, body: &str) -> Result<()> {
    let existing = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };

    let block = format!("{BLOCK_BEGIN}\n{body}\n{BLOCK_END}");

    let updated = match (existing.find(BLOCK_BEGIN), existing.find(BLOCK_END)) {
        (Some(start), Some(end)) if end > start => {
            let mut out = String::with_capacity(existing.len() + block.len());
            out.push_str(existing.get(..start).unwrap_or_default());
            out.push_str(&block);
            out.push_str(
                existing
                    .get(end.saturating_add(BLOCK_END.len())..)
                    .unwrap_or_default(),
            );
            out
        }
        // A damaged block -- one marker only, or END before BEGIN (both
        // reachable by hand-editing) -- is left alone rather than guessed at.
        // Appending would give the file two BEGINs and make every later run
        // ambiguous; rewriting could swallow the developer's own lines.
        (Some(_), None) | (None, Some(_)) | (Some(_), Some(_)) => {
            anyhow::bail!(
                "{} contains only one of the governance-auth markers, or they are out of order; \
                 refusing to guess where the managed block ends. Remove the stray marker and \
                 re-run.",
                path.display()
            )
        }
        (None, None) => {
            let mut out = existing;
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
            out.push_str(&block);
            out.push('\n');
            out
        }
    };

    // Not `write_atomically`: an rc file's existing mode is the developer's
    // business (and 0600 on a `.profile` would be a surprising side effect).
    // This file carries no secret -- only a `source` line -- precisely so it
    // doesn't need locking down.
    let tmp = path.with_extension("governance-auth-tmp");
    fs::write(&tmp, updated.as_bytes()).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Renders an absolute path under the home directory as `$HOME/...` so the
/// line written into an rc file stays correct if that file is shared between
/// machines with different usernames -- a real pattern for dotfiles repos.
pub(crate) fn display_with_home(path: &Path) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            path.strip_prefix(&home).map_or_else(
                |_| path.display().to_string(),
                |rest| format!("$HOME/{}", rest.display()),
            )
        }
        _ => path.display().to_string(),
    }
}
