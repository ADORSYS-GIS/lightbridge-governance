//! The two properties rotation has to hold: a bounded directory, and an
//! inode that survives so concurrent appenders are not orphaned.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

use super::rotate::{KEEP, MAX_BYTES, rotate};

/// A unique scratch directory without pulling in `tempfile` -- the same
/// trade `cache.rs`'s tests already make.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "governance-auth-log-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }

    fn log(&self) -> PathBuf {
        self.0.join("governance-auth.log")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn append(path: &std::path::Path, bytes: &[u8]) {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open")
        .write_all(bytes)
        .expect("append");
}

#[test]
fn rotation_keeps_the_inode_so_a_concurrent_appender_is_not_orphaned() {
    // The property that rules out `rename`. launchd holds an O_APPEND
    // handle on this file for the whole life of the drain job, and so does
    // any peer `governance-auth` already running; if rotation moved the
    // name, every one of them would keep writing into `.1` and the file a
    // human reads would stay empty for ever.
    let scratch = Scratch::new("inode");
    let path = scratch.log();
    append(&path, b"before rotation\n");

    let mut held = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .expect("peer handle");

    rotate(&path, KEEP).expect("rotate");

    held.write_all(b"after rotation\n").expect("peer append");
    let live = fs::read_to_string(&path).expect("live log");

    assert!(
        live.contains("after rotation"),
        "a writer holding the file across a rotation must keep landing in \
         the LIVE log, got {live:?}"
    );
    assert!(
        !live.contains("before rotation"),
        "the live log must actually have been emptied, got {live:?}"
    );
    assert!(
        fs::read_to_string(scratch.0.join("governance-auth.log.1"))
            .expect("generation 1")
            .contains("before rotation"),
        "the rotated generation must carry what the live log lost"
    );
}

#[test]
fn a_truncated_file_is_written_from_zero_not_from_the_old_offset() {
    // The specific hazard `docs/governance-auth/files.md` records for the
    // Copilot spool: truncating under a writer that is NOT in append mode
    // makes the next write land at the old offset with the gap zero-filled.
    // Ours are all O_APPEND, so this asserts the file has no NUL hole.
    let scratch = Scratch::new("hole");
    let path = scratch.log();
    append(&path, &vec![b'x'; 4096]);

    let mut held = OpenOptions::new().append(true).open(&path).expect("held");
    rotate(&path, KEEP).expect("rotate");
    held.write_all(b"tail").expect("append");

    let live = fs::read(&path).expect("live log");
    assert_eq!(
        live, b"tail",
        "an O_APPEND writer must resume at zero after truncation, with no \
         zero-filled hole"
    );
}

#[test]
fn the_directory_is_bounded_no_matter_how_many_rotations_run() {
    // The disk claim, asserted rather than asserted-in-prose: `KEEP`
    // generations plus the live file, and nothing beyond `.KEEP` ever
    // exists however many times rotation runs.
    let scratch = Scratch::new("bound");
    let path = scratch.log();

    for round in 0..(KEEP + 4) {
        append(&path, format!("round-{round}\n").as_bytes());
        rotate(&path, KEEP).expect("rotate");
    }

    let names: Vec<String> = fs::read_dir(&scratch.0)
        .expect("read dir")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();

    assert_eq!(
        names.len(),
        KEEP + 1,
        "the live file plus exactly {KEEP} generations, got {names:?}"
    );
    assert!(
        !names.contains(&format!("governance-auth.log.{}", KEEP + 1)),
        "a generation past the ceiling must never be created, got {names:?}"
    );
    assert!(
        fs::read_to_string(scratch.0.join("governance-auth.log.1"))
            .expect("newest generation")
            .contains(&format!("round-{}", KEEP + 3)),
        "`.1` must be the NEWEST generation, not the oldest"
    );
}

#[test]
fn the_bound_is_the_number_this_module_advertises() {
    // Guards the arithmetic in the module doc: change either constant and
    // this fails, so the documented 4 MiB cannot silently drift.
    assert_eq!(
        (KEEP as u64 + 1) * MAX_BYTES,
        4 * 1024 * 1024,
        "the log directory's advertised ceiling is 4 MiB"
    );
}
