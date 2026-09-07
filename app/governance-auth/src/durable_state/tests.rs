//! Tests for [`super`]. Split into their own file purely to keep both halves
//! under the 200-LoC gate.

use std::path::PathBuf;

use serde::Deserialize;

use super::*;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "durable-state-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("creating the scratch directory");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Sample {
    n: u64,
}

#[test]
fn missing_file_loads_as_default() {
    let dir = TempDir::new("missing");
    let path = dir.0.join("state.json");
    let loaded: Sample = load(&path).expect("a missing file is not an error");
    assert_eq!(loaded, Sample::default());
}

#[test]
fn store_then_load_round_trips() {
    let dir = TempDir::new("roundtrip");
    let path = dir.0.join("state.json");
    let value = Sample { n: 42 };
    store(&path, &value).expect("store");
    let loaded: Sample = load(&path).expect("load");
    assert_eq!(loaded, value);
}

#[test]
fn an_unparseable_file_is_fatal_not_defaulted() {
    let dir = TempDir::new("garbage");
    let path = dir.0.join("state.json");
    fs::write(&path, b"not json").expect("write garbage");
    let error = load::<Sample>(&path).expect_err("garbage must not silently become default");
    assert!(
        format!("{error:#}").contains("parsing the state file"),
        "names the condition: {error:#}"
    );
}

#[test]
fn store_leaves_no_tmp_file_behind() {
    let dir = TempDir::new("no-tmp");
    let path = dir.0.join("state.json");
    store(&path, &Sample { n: 1 }).expect("store");
    assert!(!path.with_extension("json.tmp").exists());
}

/// #291 review, P1-2: the store this module used to call
/// (`private_file::write`, no `fsync` at all) is exactly the "canonical
/// recipe for a zero-length file after a power loss" the review named. This
/// does not simulate a real crash -- nothing short of a kernel or a
/// filesystem harness can -- but it pins the mechanical fact the fix rests
/// on: the write path taken here is a *syncing* one, not the house
/// `write_private_file`'s tmp-file-only `sync_all` (this also syncs the
/// directory), so a regression back to `fs::write`/no `0o600`/no sync is at
/// least visible as a file-permissions or -content change here even though
/// the crash-survival property itself is not unit-testable.
#[cfg(unix)]
#[test]
fn the_written_file_is_private_and_survives_a_reopen() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("mode");
    let path = dir.0.join("state.json");
    store(&path, &Sample { n: 7 }).expect("store");
    let mode = fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "state file must not be group/other readable");
    // Reopening from a fresh handle (not the one `store` held) is the
    // closest a unit test gets to "durable across a restart": if the sync
    // were a no-op stub, a sufficiently small/fast write could still
    // round-trip in-process without ever having left the page cache, so
    // this does not prove durability -- it proves the write path used here
    // did not silently drop content on the way, the other half of what a
    // torn write produces.
    let reopened: Sample = load(&path).expect("reopen");
    assert_eq!(reopened, Sample { n: 7 });
}
