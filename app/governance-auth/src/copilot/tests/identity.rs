//! Which file the offset belongs to.
//!
//! These are the cases `size < offset` gets wrong, and the two it gets right
//! that must keep working.

use std::path::PathBuf;

use super::{
    super::spool::{self, Restart},
    log_line,
};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "copilot-identity-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("creating the scratch directory");
        Self(path)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn lines(count: usize) -> String {
    (0..count).map(|_| format!("{}\n", log_line())).collect()
}

/// THE case. A brand-new file that has already outgrown the old offset is
/// still a new file, and `size < offset` cannot see it.
#[test]
fn a_longer_replacement_file_is_a_rotation_even_though_it_is_longer() {
    let dir = TempDir::new("replaced");
    let path = dir.file("spool.jsonl");
    std::fs::write(&path, lines(3)).expect("writing the first spool");
    let first = spool::drain(&path, 0, None).expect("first drain");
    let identity = first
        .identity
        .expect("a present file always has an identity");

    // VS Code recreated its outfile and the developer kept working.
    std::fs::remove_file(&path).expect("removing the old spool");
    std::fs::write(&path, lines(9)).expect("writing the replacement");
    assert!(
        std::fs::metadata(&path).expect("stat").len() > first.next_offset,
        "the fixture is only meaningful when the new file has outgrown the old offset"
    );

    let after = spool::drain(&path, first.next_offset, Some(&identity)).expect("drain after");
    assert_eq!(
        after.restarted,
        Some(Restart::Replaced),
        "resuming into a file this offset was never measured against skips every record before \
         it -- undelivered and uncounted"
    );
    assert_eq!(after.lines.len(), 9, "the whole new file must be read");
}

/// The reverse risk. An identity that is absent says nothing about the file,
/// and reading "nothing" as "different" re-exports every spool on upgrade.
#[test]
fn an_absent_identity_is_adopted_rather_than_read_as_a_mismatch() {
    let dir = TempDir::new("upgrade");
    let path = dir.file("spool.jsonl");
    std::fs::write(&path, lines(4)).expect("writing the spool");
    let first = spool::drain(&path, 0, None).expect("first drain");

    let after = spool::drain(&path, first.next_offset, None).expect("drain after upgrade");

    assert_eq!(after.restarted, None);
    assert!(after.lines.is_empty(), "nothing new, so nothing re-read");
    assert!(
        after.identity.is_some(),
        "and the identity must be adopted, or the next wake asks the same unanswerable question"
    );
}

/// Appending is what this file does all day. It must never look like a
/// rotation, however much the file grows.
#[test]
fn a_file_that_merely_grew_is_not_a_rotation() {
    let dir = TempDir::new("append");
    let path = dir.file("spool.jsonl");
    std::fs::write(&path, lines(1)).expect("writing the spool");
    let first = spool::drain(&path, 0, None).expect("first drain");
    let identity = first.identity.expect("identity");

    // Well past the 4 KiB the digest covers, so the "short file digests all of
    // itself" case is exercised rather than assumed away.
    std::fs::write(&path, lines(40)).expect("growing the spool");

    let after = spool::drain(&path, first.next_offset, Some(&identity)).expect("second drain");
    assert_eq!(
        after.restarted, None,
        "the digest covers a prefix, and a prefix does not change under append"
    );
    assert_eq!(after.lines.len(), 39, "only the appended records");
}

/// Copy-truncate keeps the inode and resets the size. `size < offset` is the
/// answer there, and it must be reported as a truncation rather than as a
/// replacement -- the two send a reader looking in different places.
#[test]
fn truncation_in_place_is_reported_as_a_truncation_not_a_replacement() {
    let dir = TempDir::new("truncate");
    let path = dir.file("spool.jsonl");
    std::fs::write(&path, lines(6)).expect("writing the spool");
    let first = spool::drain(&path, 0, None).expect("first drain");
    let identity = first.identity.expect("identity");

    std::fs::write(&path, lines(1)).expect("truncating in place");

    let after = spool::drain(&path, first.next_offset, Some(&identity)).expect("second drain");
    assert_eq!(after.restarted, Some(Restart::Truncated));
    assert_eq!(after.lines.len(), 1);
}
