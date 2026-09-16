//! The on-disk session store: `<state_dir>/governance-auth/<sha256(issuer+
//! client_id)>.json`, mode `0600`, written tmp-then-rename so a reader never
//! observes a half-written file. Claude Code and Codex can both invoke the
//! `token` command around the same time on a cold store, so callers must
//! hold a [`crate::file_lock::FileLock`] across the read-refresh-write
//! critical section. (The lock used to live in this file; it was split out
//! in ADORSYS-GIS/lightbridge-governance#235 once it outlived the session
//! store itself -- `copilot push`'s drain lock and the log rotate both go
//! through it.)
//!
//! ## Why STATE, not CACHE
//!
//! This file holds a REFRESH TOKEN, so deleting it logs the developer out.
//! That makes it state by the XDG spec's own definition ("data that should
//! persist between restarts, but is not important enough to be in
//! `$XDG_DATA_HOME`"), NOT cache ("non-essential data ... can be deleted at
//! any time without loss of function").
//!
//! It used to live under `$XDG_CACHE_HOME`/`~/Library/Caches`, which is
//! actively dangerous rather than merely untidy:
//!
//! - macOS treats `~/Library/Caches` as PURGEABLE and may evict it under
//!   disk pressure, with no warning and no user action.
//! - Every "free up disk space" tool, and any container image layer that
//!   prunes `~/.cache`, does the same on Linux.
//!
//! The consequence isn't a re-login prompt at a convenient moment: `token`
//! fails closed INSIDE a running Claude Code or Codex session, and per
//! `docs/integrations/ai-client-flows.md` Codex responds to a failed helper
//! by proceeding UNAUTHENTICATED rather than stopping. Cache eviction must
//! never be able to cause that, so the session moved to state and the cache
//! directory is left for genuinely disposable things (see
//! [`crate::oauth::discovery`]).
//!
//! [`load`] migrates a session found at the legacy cache path, once.
//!
//! Storage only. WHEN a stored session still counts as usable is
//! [`crate::freshness`]'s decision, not this module's. Where the files go
//! is [`crate::paths`].

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    paths::{create_state_dir, state_dir},
    redacted::Redacted,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedSession {
    pub issuer: String,
    pub client_id: String,
    pub access_token: Redacted<String>,
    pub refresh_token: Option<Redacted<String>>,
    pub expires_at: u64,
    /// How long this access token was minted to live, as the token endpoint
    /// reported it (`expires_in`). Recorded so `crate::freshness` can tell a
    /// demand no token from this server could ever satisfy from an ordinary
    /// near-expiry one, without a second round trip.
    ///
    /// `Option` only because a session written before this field existed has
    /// none; `serde(default)` is what keeps such a file loadable rather than
    /// logging the developer out on upgrade.
    #[serde(default)]
    pub lifetime_secs: Option<u64>,
}

/// `$XDG_CACHE_HOME` (or `~/.cache`) on Linux, `~/Library/Caches` on macOS.
/// Hand-rolled rather than pulling in the `dirs` crate: `dirs` drags in
/// `dirs-sys` -> `option-ext` (MPL-2.0) on macOS/BSD, which isn't on this
/// repo's allowed-license list (`deny.toml`) -- and this repo targets only
/// macOS and Linux laptops, so the two-branch version below is the whole
/// problem.
///
/// Only the LEGACY session location now; nothing is written here. Kept so
/// [`load`] can migrate a session written by an older build, and so
/// [`clear`] can guarantee `logout` leaves no copy behind.
pub fn cache_dir() -> Result<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join("governance-auth"));
    }

    let home = std::env::var("HOME")
        .context("locating the cache directory ($XDG_CACHE_HOME and $HOME both unset)")?;
    let home = PathBuf::from(home);

    let base = if cfg!(target_os = "macos") {
        home.join("Library").join("Caches")
    } else {
        home.join(".cache")
    };
    Ok(base.join("governance-auth"))
}

fn cache_key(issuer: &str, client_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(issuer.as_bytes());
    hasher.update(b"\0");
    hasher.update(client_id.as_bytes());
    hex::encode(hasher.finalize())
}

fn session_path(issuer: &str, client_id: &str) -> Result<PathBuf> {
    Ok(state_dir()?.join(format!("{}.json", cache_key(issuer, client_id))))
}

/// Where builds before the state/cache split wrote the session.
fn legacy_session_path(issuer: &str, client_id: &str) -> Result<PathBuf> {
    Ok(cache_dir()?.join(format!("{}.json", cache_key(issuer, client_id))))
}

/// The path of the [`crate::file_lock::FileLock`] over the session file:
/// beside it, keyed the same way, so a lock outlives a re-keyed session and
/// the two can never be split across directories.
pub(crate) fn session_lock_path(issuer: &str, client_id: &str) -> Result<PathBuf> {
    Ok(state_dir()?.join(format!("{}.lock", cache_key(issuer, client_id))))
}

/// Moves a session written by an older build from the cache path to the
/// state path, once. Copy-verify-unlink rather than `fs::rename`, because
/// the two directories are frequently on different filesystems (`~/.cache`
/// vs `~/.local/state` on a laptop with a separate cache volume, and
/// container images that mount one and not the other) -- `rename` fails
/// with `EXDEV` there, and a migration that silently fails is a logout.
///
/// Failure is NOT fatal: the caller falls back to reading the legacy file
/// in place. Being unable to move a session is not a reason to log someone
/// out mid-session; it just means the migration retries next time.
fn migrate_legacy_session(legacy: &Path, target: &Path) -> Result<()> {
    let bytes = fs::read(legacy)
        .with_context(|| format!("reading legacy session at {}", legacy.display()))?;

    let dir = target
        .parent()
        .context("session path has no parent directory")?;
    create_state_dir(dir)?;

    let tmp = target.with_extension("json.tmp");
    write_private_file(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, target)
        .with_context(|| format!("renaming {} to {}", tmp.display(), target.display()))?;

    // Only unlink once the new copy is definitely readable. An interrupted
    // migration must leave the developer logged IN, never logged out.
    fs::read(target)
        .with_context(|| format!("verifying migrated session at {}", target.display()))?;
    fs::remove_file(legacy)
        .with_context(|| format!("removing legacy session at {}", legacy.display()))?;
    Ok(())
}

pub fn load(issuer: &str, client_id: &str) -> Result<Option<CachedSession>> {
    let path = session_path(issuer, client_id)?;

    // One-time migration off the old cache location. Only consulted when
    // nothing is at the new path, so it costs one `exists` check per call
    // once migrated, and never overwrites a newer session.
    if !path.exists()
        && let Ok(legacy) = legacy_session_path(issuer, client_id)
        && legacy.is_file()
    {
        match migrate_legacy_session(&legacy, &path) {
            Ok(()) => eprintln!(
                "Moved the cached session to {} (it holds a refresh token, so it must not \
                 live in a cache directory that the OS may purge).",
                path.display()
            ),
            Err(error) => {
                // Read it where it lies rather than failing: a session that
                // can't be moved is still a valid session.
                eprintln!("warning: could not migrate the session off the cache path: {error:#}");
                return read_session(&legacy);
            }
        }
    }

    read_session(&path)
}

fn read_session(path: &Path) -> Result<Option<CachedSession>> {
    match fs::read(path) {
        Ok(bytes) => {
            let session = serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing cached session at {}", path.display()))?;
            Ok(Some(session))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("reading cached session at {}", path.display()))
        }
    }
}

pub fn store(session: &CachedSession) -> Result<()> {
    let dir = state_dir()?;
    create_state_dir(&dir)?;

    let path = session_path(&session.issuer, &session.client_id)?;
    let tmp_path = path.with_extension("json.tmp");

    let bytes = serde_json::to_vec_pretty(session).context("serializing session cache")?;

    // tmp-then-rename so a reader never observes a half-written session. On
    // FAILURE the temp must not be left behind: a full disk otherwise strands a
    // zero-byte `.json.tmp` in the credential directory forever (#153), which
    // is confusing precisely when someone is already debugging a failure -- it
    // sat next to the empty lock file while #152 was being diagnosed and looked
    // like evidence.
    //
    // Cleanup is `let _ =` on purpose: it must never mask the ORIGINAL error.
    // The user needs to see `No space left on device`, not a failure to tidy up
    // after it.
    if let Err(error) = write_private_file(&tmp_path, &bytes) {
        let _ = fs::remove_file(&tmp_path);
        return Err(error).with_context(|| format!("writing {}", tmp_path.display()));
    }
    if let Err(error) = fs::rename(&tmp_path, &path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(error)
            .with_context(|| format!("renaming {} to {}", tmp_path.display(), path.display()));
    }
    Ok(())
}

/// Removes the session from BOTH the state path and the legacy cache path.
///
/// Clearing only the current path would leave a pre-migration copy — and
/// therefore a usable refresh token — sitting in `~/.cache` after `logout`
/// said "session cleared". A logout that leaves a live credential on disk
/// is worse than no logout, because it reports success.
pub fn clear(issuer: &str, client_id: &str) -> Result<()> {
    for path in [
        session_path(issuer, client_id)?,
        legacy_session_path(issuer, client_id)?,
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("removing cached session at {}", path.display()));
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique scratch directory without pulling in `tempfile` -- adding a
    /// dependency to a security-adjacent binary for one test is a poor trade.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "governance-auth-test-{}-{}-{:?}",
                tag,
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("scratch dir");
            Self(dir)
        }
        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A failed session write must not strand a `.json.tmp` in the credential
    /// directory, and the cleanup must not mask the original error.
    #[test]
    fn store_removes_its_temp_file_when_the_write_fails() {
        let s = Scratch::new("store");
        // Force `write_private_file` to fail: the target is a directory.
        let tmp = s.join("s.json.tmp");
        fs::create_dir(&tmp).expect("occupy tmp path");
        let err = write_private_file(&tmp, b"x").expect_err("writing onto a directory must fail");
        assert!(!err.to_string().is_empty());
        // With the directory removed, the same path writes and cleans normally.
        fs::remove_dir(&tmp).expect("unblock");
        write_private_file(&tmp, b"x").expect("write");
        assert!(tmp.exists());
        let _ = fs::remove_file(&tmp);
        assert!(!tmp.exists(), "temp must not survive cleanup");
    }
}
