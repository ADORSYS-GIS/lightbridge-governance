//! The regression guards that justify every branch of the lock: they came
//! with the code out of `crate::cache` and stay with it here.

use std::{
    path::PathBuf,
    time::{Duration, SystemTime},
};

use super::*;

/// A unique scratch directory without pulling in `tempfile` -- adding a
/// dependency to a security-adjacent binary for these tests is a poor trade.
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
