//! One drain at a time.
//!
//! Read the checkpoint, drain from that offset, POST, write the checkpoint
//! back: that is a read-modify-write over one file, and every byte of it has
//! to be under a single writer. Without one, two runs read the same offset,
//! ship the same records, and both write a checkpoint that looks perfectly
//! normal afterwards -- the duplication is invisible from this side and shows
//! up as inflated usage at the collector.
//!
//! Not hypothetical, and not rare: `status` tells the developer to run
//! `governance-auth copilot-push` by hand precisely when there is a backlog,
//! and the five-minute timer that also runs it has no idea. Three concurrent
//! processes on a two-record spool sent every record three times.
//!
//! ## Why not the session lock
//!
//! [`crate::cache::FileLock`] already guards the session file, and
//! `oauth::current_session` takes it -- but drops it on return, long before
//! the spool is opened. It is also keyed on issuer/client, and the spool
//! belongs to the machine's VS Code install rather than to whichever identity
//! happens to be pushing it, so two identities draining one spool would take
//! two different locks. This one is keyed on the checkpoint, like the
//! checkpoint file itself.

use std::path::Path;

use anyhow::{Context, Result};

use crate::cache::FileLock;

/// Sits beside `copilot-push.json`, named after it for the same reason it is:
/// the thing being guarded is the spool's progress, not a session.
const FILE_NAME: &str = "copilot-push.lock";

/// Blocks until no other drain is running.
///
/// Waiting rather than exiting is deliberate. The overlapping cases are a
/// timer wake and an impatient human, and for both the useful outcome is "your
/// run happened, just after the other one" -- a run that exits because another
/// held the lock would report failure for a situation that resolves itself in
/// seconds. Stale locks are reclaimed by PID liveness, so a crashed drain does
/// not block the next one; see [`FileLock`].
pub fn acquire(state_dir: &Path) -> Result<FileLock> {
    FileLock::acquire_at(state_dir.join(FILE_NAME))
        .context("waiting for another `copilot-push` to finish")
}
