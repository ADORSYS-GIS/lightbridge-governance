//! Per-file drift reporting. Split from the parent test file to keep both
//! under the 200-LoC gate.

use super::*;

/// An empty manifest means `configure` has not run. Saying so, with the command
/// to fix it, beats an empty table the reader has to interpret.
#[test]
fn drift_is_reported_per_target_without_alarm() {
    let targets = vec![
        target("/home/dev/.claude/settings.json", 12, 0),
        target("/home/dev/.codex/config.toml", 11, 2),
    ];
    let out = render(
        "https://auth.example",
        "cli",
        &session(true, true),
        &otel(None, false),
        &targets,
    );

    assert!(out.contains("12 keys managed"), "{out}");
    assert!(out.contains("11 keys managed"), "{out}");
    // Wording matters: a key the developer changed is not an error, and the
    // table must not imply we are about to do something about it.
    assert!(out.contains("2 changed by you, left alone"), "{out}");
    assert!(
        !out.to_lowercase().contains("error") && !out.to_lowercase().contains("warn"),
        "drift is not a fault: {out}"
    );
}

/// Counting drift means reading the target files, so it must survive a target
/// that has been deleted since `configure` ran -- an uninstalled tool.
#[test]
fn a_deleted_target_does_not_panic_or_vanish() {
    let dir = std::env::temp_dir().join(format!("gauth-dash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".config/governance-auth")).expect("dirs");

    let manifest = format!(
        r#"{{"version":1,"targets":{{"{}":{{"a":"deadbeef"}}}}}}"#,
        dir.join("gone.json").display()
    );
    std::fs::write(dir.join(".config/governance-auth/managed.json"), manifest)
        .expect("seed manifest");

    let found = targets(&dir);
    assert_eq!(found.len(), 1, "a vanished target must still be listed");
    assert_eq!(found[0].managed, 1);
    assert_eq!(found[0].edited, 0, "absent file is not 'edited by you'");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Paths shorten for readability, and a path outside home passes through
/// rather than being mangled.
#[test]
fn paths_shorten_to_tilde_only_under_home() {
    let home = Path::new("/home/dev");
    assert_eq!(
        short("/home/dev/.claude/settings.json", home),
        "~/.claude/settings.json"
    );
    assert_eq!(
        short("/etc/governance-auth/config.toml", home),
        "/etc/governance-auth/config.toml"
    );
}
