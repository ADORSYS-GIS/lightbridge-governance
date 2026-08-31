//! The checkpoint file itself.

use std::path::PathBuf;

use super::super::checkpoint;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "copilot-checkpoint-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("creating the scratch directory");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_missing_checkpoint_means_never_drained() {
    let dir = TempDir::new("missing");
    let state = checkpoint::load(&checkpoint::path(&dir.0)).expect("a missing checkpoint is ok");
    assert_eq!(state.offset, 0);
    assert_eq!(state.last_push_unix, None, "never pushed, not 'pushed now'");
}

#[test]
fn a_stored_checkpoint_round_trips() {
    let dir = TempDir::new("roundtrip");
    let path = checkpoint::path(&dir.0);
    let written = checkpoint::Checkpoint {
        offset: 4096,
        last_push_unix: Some(1_788_191_916),
        last_push_records: 12,
    };

    checkpoint::store(&path, &written).expect("storing");
    let read = checkpoint::load(&path).expect("loading");

    assert_eq!(read.offset, 4096);
    assert_eq!(read.last_push_unix, Some(1_788_191_916));
    assert_eq!(read.last_push_records, 12);
    assert!(
        !path.with_extension("json.tmp").exists(),
        "the tmp file must be renamed away, not left behind"
    );
}

#[test]
fn an_unreadable_checkpoint_is_an_error_not_a_silent_restart() {
    let dir = TempDir::new("corrupt");
    let path = checkpoint::path(&dir.0);
    std::fs::write(&path, b"{ this is not json").expect("planting a corrupt checkpoint");

    let error = checkpoint::load(&path)
        .expect_err("defaulting to offset 0 here would silently re-push everything already sent");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("Delete it"),
        "the error must say what to do about it, got: {rendered}"
    );
}

#[cfg(unix)]
#[test]
fn the_checkpoint_is_written_private() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("perms");
    let path = checkpoint::path(&dir.0);
    checkpoint::store(&path, &checkpoint::Checkpoint::default()).expect("storing");

    let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
    assert_eq!(
        mode & 0o077,
        0,
        "it sits beside the session files; it must not be the one readable file in there"
    );
}
