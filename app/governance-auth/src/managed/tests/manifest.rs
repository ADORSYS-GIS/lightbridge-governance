//! Persistence and secrecy properties of the managed-key manifest.

use std::collections::BTreeMap;

use super::super::{testutil::*, *};

/// Codex's block contains `Authorization = "Bearer <token>"`. Recording values
/// rather than digests would copy that credential into a second file.
#[test]
fn the_manifest_never_contains_a_secret() {
    let dir = tempdir();
    let path = dir.path().join("managed.json");
    let manifest = previous(
        Path::new("/x/config.toml"),
        &[("a.b", "Bearer SUPER-SECRET")],
    );
    save(&path, &manifest).expect("save");

    let text = fs::read_to_string(&path).expect("read");
    assert!(!text.contains("SUPER-SECRET"), "secret persisted:\n{text}");
    assert_eq!(load(&path), manifest, "must round-trip");
}

/// Losing the manifest must never block `configure` -- it is bookkeeping.
#[test]
fn a_missing_or_corrupt_manifest_is_empty_not_an_error() {
    let dir = tempdir();
    assert_eq!(load(&dir.path().join("absent.json")), Manifest::default());

    let corrupt = dir.path().join("corrupt.json");
    fs::write(&corrupt, "{not json").expect("seed");
    assert_eq!(load(&corrupt), Manifest::default());

    let future = dir.path().join("future.json");
    fs::write(&future, r#"{"version":999,"targets":{}}"#).expect("seed");
    assert_eq!(load(&future), Manifest::default(), "unknown version");
}

/// A target that no longer exists is not recreated to delete a key from it.
#[test]
fn a_vanished_target_is_left_alone() {
    let dir = tempdir();
    let target = dir.path().join("gone.json");
    let manifest = previous(&target, &[("k", "v")]);
    assert!(
        retract_stale(&manifest, &BTreeMap::new())
            .expect("retract")
            .is_empty()
    );
    assert!(!target.exists(), "must not recreate the file");
}
