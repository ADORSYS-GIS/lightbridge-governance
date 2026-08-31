//! Tests for [`super`].

use std::collections::BTreeMap;

use super::{testutil::*, *};

/// The point of the whole module: a key we wrote and no longer write goes away.
#[test]
fn a_key_we_stopped_writing_is_removed() {
    let dir = tempdir();
    let target = dir.path().join("settings.json");
    fs::write(
        &target,
        r#"{"apiKeyHelper":"gauth token","OTEL_EXPORTER_OTLP_HEADERS":"Bearer x","mine":1}"#,
    )
    .expect("seed");

    let manifest = previous(
        &target,
        &[
            ("apiKeyHelper", "gauth token"),
            ("OTEL_EXPORTER_OTLP_HEADERS", "Bearer x"),
        ],
    );
    // This run writes only apiKeyHelper.
    let mut now = BTreeMap::new();
    let mut keeping = BTreeMap::new();
    keeping.insert("apiKeyHelper".to_owned(), digest("gauth token"));
    now.insert(target.display().to_string(), keeping);

    let removed = retract_stale(&manifest, &now).expect("retract");
    let after: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&target).expect("read")).expect("json");

    assert!(after.get("OTEL_EXPORTER_OTLP_HEADERS").is_none(), "{after}");
    assert!(
        after.get("apiKeyHelper").is_some(),
        "still-written key gone"
    );
    assert!(after.get("mine").is_some(), "developer's key touched");
    assert_eq!(removed.len(), 1, "should report what it deleted");
}

/// The mitigation for the risk a side manifest carries. If the developer has
/// changed the value since we wrote it, the key is theirs now -- deleting it
/// would destroy their work, which is worse than the stale key.
#[test]
fn a_developer_edited_value_is_never_removed() {
    let dir = tempdir();
    let target = dir.path().join("settings.json");
    fs::write(&target, r#"{"apiKeyHelper":"MY OWN COMMAND"}"#).expect("seed");

    // We recorded writing something else entirely.
    let manifest = previous(&target, &[("apiKeyHelper", "gauth token")]);
    let removed = retract_stale(&manifest, &BTreeMap::new()).expect("retract");

    let after: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&target).expect("read")).expect("json");
    assert_eq!(
        after.get("apiKeyHelper").and_then(|v| v.as_str()),
        Some("MY OWN COMMAND"),
        "edited value must survive"
    );
    assert!(removed.is_empty(), "nothing should be reported removed");
}

/// VS Code's settings.json uses flat keys containing dots. Splitting on `.`
/// first would never find them, so they would never be retracted -- and the
/// failure would be silent.
#[test]
fn flat_dotted_keys_are_found_before_nesting() {
    let dir = tempdir();
    let target = dir.path().join("settings.json");
    fs::write(
        &target,
        r#"{"github.copilot.chat.otel.otlpEndpoint":"https://otel.example"}"#,
    )
    .expect("seed");

    let key = "github.copilot.chat.otel.otlpEndpoint";
    let manifest = previous(&target, &[(key, "https://otel.example")]);
    let removed = retract_stale(&manifest, &BTreeMap::new()).expect("retract");

    let after: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&target).expect("read")).expect("json");
    assert!(after.get(key).is_none(), "flat dotted key kept: {after}");
    assert_eq!(removed.len(), 1);
}

/// Claude Code's `env.X` genuinely IS nested, so the fallback must still work.
#[test]
fn nested_keys_still_resolve() {
    let dir = tempdir();
    let target = dir.path().join("settings.json");
    fs::write(&target, r#"{"env":{"STALE":"v","KEPT":"k"}}"#).expect("seed");

    let manifest = previous(&target, &[("env.STALE", "v")]);
    retract_stale(&manifest, &BTreeMap::new()).expect("retract");

    let after: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&target).expect("read")).expect("json");
    assert!(after["env"].get("STALE").is_none(), "{after}");
    assert!(after["env"].get("KEPT").is_some(), "sibling removed too");
}

/// ⚠️ Documents a KNOWN LIMITATION, not a desired property.
///
/// In `toml_edit` a comment above a key is that key's leading decor, so
/// removing the key removes the comment with it. For our own banner that is
/// what we want. For a comment the developer wrote immediately above one of
/// our keys, it is a small loss -- bounded, because it only affects comments
/// directly above a key this binary owns, and never anything elsewhere in the
/// file. Carrying the decor forward to the next key would fix it; see #210.
#[test]
fn removing_a_toml_key_also_takes_the_comment_above_it() {
    let dir = tempdir();
    let target = dir.path().join("config.toml");
    fs::write(
        &target,
        "# above ours\nmodel_provider = \"governance\"\nkeep = 1\n",
    )
    .expect("seed");

    let manifest = previous(&target, &[("model_provider", "governance")]);
    retract_stale(&manifest, &BTreeMap::new()).expect("retract");

    let after = fs::read_to_string(&target).expect("read");
    assert!(!after.contains("model_provider"), "{after}");
    assert!(
        !after.contains("# above ours"),
        "if this now passes, the limitation is fixed -- update the doc and #210: {after}"
    );
    // What must NOT change: anything not attached to the key we removed.
    assert!(after.contains("keep = 1"), "unrelated key removed: {after}");
}

/// The bound on that limitation: a comment attached to a DIFFERENT key is safe.
#[test]
fn comments_on_other_keys_survive_retraction() {
    let dir = tempdir();
    let target = dir.path().join("config.toml");
    fs::write(
        &target,
        "model_provider = \"governance\"\n\n# the developer's note\nkeep = 1\n",
    )
    .expect("seed");

    let manifest = previous(&target, &[("model_provider", "governance")]);
    retract_stale(&manifest, &BTreeMap::new()).expect("retract");

    let after = fs::read_to_string(&target).expect("read");
    assert!(after.contains("# the developer's note"), "{after}");
    assert!(after.contains("keep = 1"), "{after}");
}

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
