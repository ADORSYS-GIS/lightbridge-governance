//! `otel_token`'s two read-side permission checks -- split out of
//! `config_file/mod.rs` (issue #364) purely to keep that file under its
//! grandfathered LoC ceiling after adding the callback-port/callback-bind
//! fields; unrelated to either.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};

/// Reads the secret a `otel_token_file = "/path"` entry points at, refusing
/// first if that file itself is readable by group or other -- it carries
/// exactly the same secret as an inlined `otel_token`, so it gets exactly
/// the same check.
///
/// Trailing newline is stripped: the common way to produce one of these
/// files (`echo "$TOKEN" > path`, an ESO `secretKeyRef` volume mount) always
/// leaves one, and a token with a literal trailing `\n` baked into every
/// `Authorization` header would fail at the collector in a way that's
/// miserable to debug.
pub(super) fn read_token_file(path: &Path) -> Result<String> {
    refuse_if_group_or_other_readable(path)?;
    let contents = fs::read_to_string(path)
        .with_context(|| format!("reading otel_token_file at {}", path.display()))?;
    let token = contents.trim_end_matches(['\n', '\r']).to_owned();
    if token.is_empty() {
        bail!("otel_token_file at {} is empty", path.display());
    }
    Ok(token)
}

/// The SSH-precedent permission check: refuse to load a file that carries a
/// secret if its mode grants group or other any permission at all, and name
/// the exact fix rather than making the operator work it out. Mirrors the
/// posture `otel.rs` already takes when it *writes* `otel.env` at `0600`;
/// this is the read-side equivalent for a file this binary didn't write
/// itself and can't assume the permissions of.
#[cfg(unix)]
pub(super) fn refuse_if_group_or_other_readable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(path)
        .with_context(|| format!("stat-ing {}", path.display()))?
        .permissions()
        .mode();

    if mode & 0o077 != 0 {
        bail!(
            "{} carries an OTLP ingest token and is readable by group or other (mode {:o}); \
             refusing to load it. Fix with:\n\n  chmod 600 {}\n",
            path.display(),
            mode & 0o777,
            path.display(),
        );
    }
    Ok(())
}

/// Non-Unix targets have no POSIX mode bits to check. This binary only ships
/// for Linux and macOS (ADR-0012 §1), so this arm exists only so the crate
/// still compiles if that ever changes, not because it's expected to run.
#[cfg(not(unix))]
pub(super) fn refuse_if_group_or_other_readable(_path: &Path) -> Result<()> {
    Ok(())
}
