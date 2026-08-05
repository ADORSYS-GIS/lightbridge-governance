//! The on-disk session cache: `<cache_dir>/governance-auth/<sha256(issuer+
//! client_id)>.json`, mode `0600`, written tmp-then-rename so a reader never
//! observes a half-written file. Claude Code and Codex can both invoke the
//! `token` command around the same time on a cold cache, so callers must
//! hold a [`FileLock`] across the read-refresh-write critical section.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How far ahead of the real expiry a cached token is treated as unusable.
/// Matches the margin the org's own `opencode-oauth2` plugin uses
/// (`tokenExpirySkewMs`), so a caller with a short re-check interval (Codex's
/// `refresh_interval_ms`) never hands a token to the tool a moment before
/// Keycloak would reject it.
const SKEW_SECONDS: u64 = 30;

const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(200);
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedSession {
    pub issuer: String,
    pub client_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: u64,
}

impl CachedSession {
    pub fn is_fresh(&self) -> Result<bool> {
        Ok(self.expires_at > now_unix()?.saturating_add(SKEW_SECONDS))
    }

    pub fn seconds_until_expiry(&self) -> Result<i64> {
        Ok(i64::try_from(self.expires_at)
            .unwrap_or(i64::MAX)
            .saturating_sub(i64::try_from(now_unix()?).unwrap_or(i64::MAX)))
    }
}

fn now_unix() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("reading system clock")?
        .as_secs())
}

/// `$XDG_CACHE_HOME` (or `~/.cache`) on Linux, `~/Library/Caches` on macOS.
/// Hand-rolled rather than pulling in the `dirs` crate: `dirs` drags in
/// `dirs-sys` -> `option-ext` (MPL-2.0) on macOS/BSD, which isn't on this
/// repo's allowed-license list (`deny.toml`) -- and this repo targets only
/// macOS and Linux laptops, so the two-branch version below is the whole
/// problem.
fn cache_dir() -> Result<PathBuf> {
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
    Ok(cache_dir()?.join(format!("{}.json", cache_key(issuer, client_id))))
}

fn lock_path(issuer: &str, client_id: &str) -> Result<PathBuf> {
    Ok(cache_dir()?.join(format!("{}.lock", cache_key(issuer, client_id))))
}

pub fn load(issuer: &str, client_id: &str) -> Result<Option<CachedSession>> {
    let path = session_path(issuer, client_id)?;
    match fs::read(&path) {
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
    let dir = cache_dir()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating cache directory {}", dir.display()))?;

    let path = session_path(&session.issuer, &session.client_id)?;
    let tmp_path = path.with_extension("json.tmp");

    let bytes = serde_json::to_vec_pretty(session).context("serializing session cache")?;
    write_private_file(&tmp_path, &bytes)
        .with_context(|| format!("writing {}", tmp_path.display()))?;
    fs::rename(&tmp_path, &path)
        .with_context(|| format!("renaming {} to {}", tmp_path.display(), path.display()))?;
    Ok(())
}

pub fn clear(issuer: &str, client_id: &str) -> Result<()> {
    let path = session_path(issuer, client_id)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("removing cached session at {}", path.display()))
        }
    }
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

/// A coarse mutex over the session file, implemented as a create-new
/// sentinel file rather than pulling in an flock crate for one call site.
/// Stale-lock recovery: a lock older than [`LOCK_TIMEOUT`] is assumed to be
/// left over from a crashed process and is removed.
pub struct FileLock {
    path: PathBuf,
}

impl FileLock {
    pub fn acquire(issuer: &str, client_id: &str) -> Result<Self> {
        let dir = cache_dir()?;
        fs::create_dir_all(&dir)
            .with_context(|| format!("creating cache directory {}", dir.display()))?;
        let path = lock_path(issuer, client_id)?;
        let deadline = Instant::now() + LOCK_TIMEOUT;

        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Instant::now() >= deadline {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    std::thread::sleep(LOCK_POLL_INTERVAL);
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("acquiring session lock {}", path.display()));
                }
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
