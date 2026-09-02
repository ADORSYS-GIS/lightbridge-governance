//! The on-disk session store: `<state_dir>/governance-auth/<sha256(issuer+
//! client_id)>.json`, mode `0600`, written tmp-then-rename so a reader never
//! observes a half-written file. Claude Code and Codex can both invoke the
//! `token` command around the same time on a cold store, so callers must
//! hold a [`FileLock`] across the read-refresh-write critical section.
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
//! [`crate::freshness`]'s decision, not this module's.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::redacted::Redacted;

const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Fallback ceiling used ONLY when this process genuinely can't determine
/// whether the recorded holder is alive (an unreadable/unparseable lock
/// file, or the liveness check itself couldn't run -- see
/// [`holder_liveness`]). A *confirmed* live holder is never preempted by
/// this: an interactive `login` can legitimately hold the lock across an
/// entire browser flow, and forcing it out would break the single-writer
/// guarantee the lock exists for. This is generous because it only fires
/// in the ambiguous case, not the common one.
const LOCK_MAX_WAIT: Duration = Duration::from_secs(300);
/// How long an EMPTY lock file is presumed to be a holder mid-write rather
/// than debris. See [`holder_liveness`] -- this closes the window in which a
/// loser reads the winner's lock between `create_new` and the PID write.
const EMPTY_LOCK_GRACE: Duration = Duration::from_secs(2);

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
fn create_state_dir(dir: &Path) -> Result<()> {
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
fn create_state_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating state directory {}", dir.display()))
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

fn lock_path(issuer: &str, client_id: &str) -> Result<PathBuf> {
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

/// A coarse mutex over the session file, implemented as a create-new
/// sentinel file (containing the holder's PID) rather than pulling in an
/// flock crate for one call site. Stale-lock recovery is PID-liveness-based,
/// not timeout-based: a lock whose recorded PID is no longer running is
/// reclaimed immediately, so a legitimately slow holder (an interactive
/// `login` waiting on a human) is never preempted just because time passed.
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
    pid: u32,
}

impl FileLock {
    pub fn acquire(issuer: &str, client_id: &str) -> Result<Self> {
        Self::acquire_at(lock_path(issuer, client_id)?, None)
    }

    /// The same lock over an arbitrary state file. Exists so `copilot push`
    /// can hold ONE writer over its own read-drain-post-write critical
    /// section without a second, subtly different implementation of stale-lock
    /// recovery -- that logic is the part worth having exactly one of.
    ///
    /// `live_holder_ceiling` is how long to keep waiting on a holder that is
    /// **confirmed alive**. `None` -- what `login`/`token` pass -- waits
    /// indefinitely, because an interactive login legitimately runs for as
    /// long as a human takes and preempting it would break the single-writer
    /// guarantee. A caller on a timer wants a ceiling instead: a wake that
    /// gives up is one lost wake, whereas a wake that waits for ever behind a
    /// stuck peer is a permanently stuck drain. Reaching the ceiling is an
    /// ERROR, never a reclaim -- the holder is alive and its lock is valid.
    pub fn acquire_at(path: PathBuf, live_holder_ceiling: Option<Duration>) -> Result<Self> {
        // Must be the STATE dir, not the cache dir: every lock this takes
        // lives beside the file it guards, and creating the wrong directory
        // here would leave the lock's own parent missing.
        let dir = state_dir()?;
        create_state_dir(&dir)?;
        let pid = std::process::id();
        let started = Instant::now();
        let deadline = started + LOCK_MAX_WAIT;

        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    // ⚠️ NOT best-effort, and the previous comment here
                    // arguing it could be was exactly backwards.
                    //
                    // A lock carrying a PID can be proved abandoned in
                    // microseconds. A lock carrying NO pid can never be proved
                    // abandoned, so every later caller falls into the
                    // "undeterminable" branch and waits out LOCK_MAX_WAIT --
                    // five minutes, on a command Claude Code and Codex invoke
                    // from a timer. That ceiling is meant to be the rare
                    // fallback, not the routine outcome of a failed write.
                    //
                    // Reproduced for real: a full disk made this write fail,
                    // and every subsequent `token` blocked for 300s behind a
                    // zero-byte lock that looked perfectly normal (#152).
                    //
                    // So: if we cannot record ownership, we do not own it.
                    // Drop the file so the next caller gets a clean
                    // `create_new` rather than an unattributable wait.
                    if let Err(error) = write!(file, "{pid}").and_then(|()| file.sync_all()) {
                        drop(file);
                        let _ = fs::remove_file(&path);
                        return Err(error).with_context(|| {
                            format!(
                                "recording lock ownership in {} (lock released rather than left \
                                 un-attributable)",
                                path.display()
                            )
                        });
                    }
                    return Ok(Self { path, pid });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    match holder_liveness(&path) {
                        Some(false) => {
                            // Confirmed dead -- reclaim now, regardless of
                            // how long that took to determine.
                            let _ = fs::remove_file(&path);
                            continue;
                        }
                        Some(true) => {
                            // Confirmed alive -- keep waiting. The lock is
                            // NEVER stolen here: an interactive `login` can
                            // legitimately run for minutes, and preempting it
                            // would break the single-writer guarantee this
                            // lock exists for, on a real cadence (an
                            // automated `token` re-invoke racing a slow human
                            // login).
                            //
                            // A caller that supplied a ceiling gives up
                            // instead, and says so. That is the difference
                            // between one lost wake and a drain wedged for
                            // ever behind a peer stuck on a socket.
                            if let Some(ceiling) = live_holder_ceiling
                                && started.elapsed() >= ceiling
                            {
                                return Err(anyhow::anyhow!(
                                    "{} is still held by a live process after {}s. Giving up \
                                     rather than waiting indefinitely -- the lock is valid, so it \
                                     was not taken. Check whether an earlier run is stuck.",
                                    path.display(),
                                    ceiling.as_secs()
                                ));
                            }
                            std::thread::sleep(LOCK_POLL_INTERVAL);
                        }
                        None => {
                            // Genuinely undeterminable (unreadable lock
                            // file, or the liveness check itself couldn't
                            // run) -- this is the only case the timeout
                            // ceiling applies to.
                            if Instant::now() >= deadline {
                                let _ = fs::remove_file(&path);
                                continue;
                            }
                            std::thread::sleep(LOCK_POLL_INTERVAL);
                        }
                    }
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("acquiring session lock {}", path.display()));
                }
            }
        }
    }
}

/// Liveness of the lock file's recorded holder: `Some(true)` = confirmed
/// running, `Some(false)` = confirmed gone, `None` = couldn't tell (an
/// unreadable/unparseable lock file, or the check itself couldn't run).
/// Only the `None` case falls back to [`LOCK_MAX_WAIT`] -- see
/// [`FileLock::acquire`].
fn holder_liveness(path: &Path) -> Option<bool> {
    let contents = fs::read_to_string(path).ok()?;
    let trimmed = contents.trim();
    // An EMPTY lock is confirmed-dead, not undeterminable (#152) -- a zero-byte
    // file left behind is debris from a process that died between creating the
    // lock and recording its PID, and reclaiming it at once is what stops every
    // later caller waiting out LOCK_MAX_WAIT.
    //
    // ⚠️ With ONE exception, and it is not theoretical. The winner creates the
    // file and then writes its PID, and a loser's `create_new` fails at exactly
    // the instant the file appears -- so the loser reads it in precisely the
    // window where it is legitimately empty, calls the live holder dead, and
    // deletes the lock out from under it. Both then hold it. Reproduced: three
    // concurrent `copilot push` runs, two of which drained the same offset and
    // exported every record twice.
    //
    // So a BRAND NEW empty lock is presumed to be mid-write. This is not the
    // timeout #152 removed: it is two seconds against a window measured in
    // microseconds, it costs a crashed run's debris one extra poll rather than
    // five minutes, and it applies only while the file is both empty and newer
    // than the grace.
    if trimmed.is_empty() {
        return Some(written_within(path, EMPTY_LOCK_GRACE));
    }
    // Deliberately narrower than "anything unparseable": a lock containing
    // NON-empty garbage is still `None`, because that could be a live holder
    // whose PID we simply cannot read, and preempting a live `login` mid-browser
    // flow would break the single-writer guarantee this lock exists for.
    let pid: u32 = trimmed.parse().ok()?;
    process_is_alive(pid)
}

/// Whether `path` was last written less than `grace` ago. A clock that cannot
/// be read, or a timestamp in the future, answers `false` -- "I cannot show
/// this is mid-write" -- so an unreadable mtime never grants an empty lock
/// indefinite protection.
fn written_within(path: &Path, grace: Duration) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|written| SystemTime::now().duration_since(written).ok())
        .is_some_and(|age| age < grace)
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> Option<bool> {
    // `kill -0` checks for existence (and permission) without sending a
    // signal. Shelling out avoids a new libc/nix dependency for this one
    // call site. A failure to even run the command (not "ran and said
    // gone") is the undeterminable case, not a confirmed answer.
    match std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
    {
        Ok(status) => Some(status.success()),
        Err(_) => None,
    }
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> Option<bool> {
    None
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Only remove the lock file if it still records our own PID --
        // guards against deleting a replacement lock a peer created after
        // reclaiming what it believed (correctly or not) was an abandoned
        // lock while we were still shutting down.
        if let Ok(contents) = fs::read_to_string(&self.path)
            && contents.trim().parse::<u32>() == Ok(self.pid)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique scratch directory without pulling in `tempfile` -- adding a
    /// dependency to a security-adjacent binary for four tests is a poor trade.
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

    fn lock_containing(tag: &str, body: &str) -> (Scratch, PathBuf) {
        let s = Scratch::new(tag);
        let path = s.join("x.lock");
        fs::write(&path, body).expect("write lock");
        (s, path)
    }

    /// Debris, as a crashed run leaves it: empty and no longer being written.
    /// Backdated rather than slept on, so the test costs nothing.
    fn abandoned_lock(tag: &str, body: &str) -> (Scratch, PathBuf) {
        let (s, path) = lock_containing(tag, body);
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("reopen lock");
        file.set_modified(SystemTime::now() - EMPTY_LOCK_GRACE - Duration::from_secs(1))
            .expect("backdate lock");
        (s, path)
    }

    /// THE regression guard for the 300s block. An empty lock is exactly what a
    /// crashed or disk-full `acquire` leaves behind, and treating it as
    /// "undeterminable" sent every later `token` into LOCK_MAX_WAIT -- five
    /// minutes, on a command Claude Code and Codex invoke from a timer.
    #[test]
    fn an_empty_lock_is_confirmed_dead_not_undeterminable() {
        let (_s, path) = abandoned_lock("empty", "");
        assert_eq!(
            holder_liveness(&path),
            Some(false),
            "a zero-byte lock cannot represent a live holder; returning None here sends the \
             caller into the 300s LOCK_MAX_WAIT branch"
        );
    }

    /// The other side of the same rule. The winner creates the lock and then
    /// writes its PID; a loser's `create_new` fails at exactly that instant, so
    /// it reads the file in the one window where empty is legitimate. Calling
    /// that dead deletes a live holder's lock -- measured as three concurrent
    /// `copilot push` runs exporting every record twice.
    #[test]
    fn a_just_created_empty_lock_is_presumed_mid_write_not_dead() {
        let (_s, path) = lock_containing("racing", "");
        assert_eq!(
            holder_liveness(&path),
            Some(true),
            "an empty lock written microseconds ago is a holder between `create_new` and its PID \
             write, and reclaiming it hands two processes the same lock"
        );
    }

    #[test]
    fn a_whitespace_only_lock_is_also_confirmed_dead() {
        let (_s, path) = abandoned_lock("ws", "  \n ");
        assert_eq!(holder_liveness(&path), Some(false));
    }

    /// Deliberately NARROWER than "anything unparseable". Non-empty garbage
    /// could be a live holder whose pid we merely cannot read, and preempting a
    /// live interactive `login` mid-browser-flow would break the single-writer
    /// guarantee the lock exists for.
    #[test]
    fn a_lock_with_unreadable_but_non_empty_contents_stays_undeterminable() {
        let (_s, path) = lock_containing("garbage", "not-a-pid");
        assert_eq!(
            holder_liveness(&path),
            None,
            "non-empty garbage must NOT be reclaimed immediately -- that would preempt a \
             possibly-live holder"
        );
    }

    #[test]
    fn a_lock_held_by_this_live_process_is_reported_alive() {
        let (_s, path) = lock_containing("live", &std::process::id().to_string());
        assert_eq!(holder_liveness(&path), Some(true));
    }

    /// A confirmed-live holder is never preempted, so a caller with nothing to
    /// wait for -- `copilot push` on a timer -- has to be able to give up
    /// instead. Without a ceiling, one drain stuck on a socket wedges every
    /// later wake behind it for ever, which is a permanently stuck drain
    /// rather than one lost wake.
    #[test]
    fn a_ceiling_gives_up_on_a_live_holder_rather_than_waiting_for_ever() {
        let (_s, path) = lock_containing("ceiling", &std::process::id().to_string());
        let started = Instant::now();
        let error = FileLock::acquire_at(path.clone(), Some(Duration::from_millis(300)))
            .expect_err("a live holder must not be preempted, so this cannot succeed");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "waited {:?}; with no ceiling this blocks until the holder exits",
            started.elapsed()
        );
        assert!(
            error.to_string().contains("still held by a live process"),
            "the error must say what happened: {error:#}"
        );
        assert!(
            path.exists(),
            "giving up must NOT delete a valid lock -- the holder is alive and still writing"
        );
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
