//! A coarse write lock over shared state files.
//!
//! [`FileLock`] started life inside `crate::cache`, guarding one thing: the
//! session file's read-refresh-write critical section. It was split out
//! (ADORSYS-GIS/lightbridge-governance#235) because it had grown a second,
//! unrelated caller -- `copilot push`'s spool drain (`crate::copilot::lock`)
//! -- and a third (`crate::logging::rotate`), so the lock stopped being about
//! sessions. The session *store* and the lock over it are separate concerns
//! that happened to share a file; the stale-lock reasoning below is the part
//! worth having exactly one of.
//!
//! Configuration and paths live elsewhere: where the files go is
//! [`crate::paths`], and whether a stored session still counts as usable is
//! [`crate::freshness`]'s decision.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result};

use crate::paths::{create_state_dir, state_dir};

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

/// A coarse mutex over a state file, implemented as a create-new
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
    /// The lock over the session file, keyed on issuer and client. See
    /// [`crate::cache`] for why that sits in the state directory.
    pub fn acquire(issuer: &str, client_id: &str) -> Result<Self> {
        Self::acquire_at(crate::cache::session_lock_path(issuer, client_id)?, None)
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
mod tests;
