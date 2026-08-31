//! Tests for [`super`].

use std::path::Path;

use super::*;

fn target(path: &str, managed: usize, edited: usize) -> Target {
    Target {
        path: path.to_owned(),
        managed,
        edited,
    }
}

fn session(cached: bool, fresh: bool) -> Session {
    Session {
        cached,
        fresh,
        expires_in: 900,
    }
}

/// The three documented lines are a surface other things depend on --
/// `commands.md` lists them and `cli_arg_order.rs` asserts one. The dashboard
/// is an addition for a human, never a replacement.
#[test]
fn plain_output_is_unchanged() {
    assert_eq!(
        plain(&session(true, true)),
        "session cached, fresh, expires in 900s"
    );
    assert_eq!(
        plain(&session(true, false)),
        "session cached, needs refresh, expires in 900s"
    );
    assert_eq!(plain(&session(false, false)), "no cached session");
}

#[test]
fn the_table_reports_the_session_state() {
    let fresh = render("https://auth.example", "cli", &session(true, true), &[]);
    assert!(fresh.contains("fresh"), "{fresh}");
    assert!(fresh.contains("900s left"), "{fresh}");

    let none = render("https://auth.example", "cli", &session(false, false), &[]);
    assert!(none.contains("no cached session"), "{none}");
}

/// An empty manifest means `configure` has not run. Saying so, with the command
/// to fix it, beats an empty table the reader has to interpret.
#[test]
fn nothing_configured_says_what_to_do() {
    let out = render("https://auth.example", "cli", &session(true, true), &[]);
    assert!(out.contains("nothing yet"), "{out}");
    assert!(out.contains("governance-auth configure"), "{out}");
}

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

/// Found by looking at the output, not by reasoning: padding every value left
/// trailing spaces on most rows, which survive copy-paste into an issue.
#[test]
fn no_row_has_trailing_whitespace() {
    let targets = vec![target("~/.codex/config.toml", 11, 2)];
    for out in [
        render(
            "https://auth.example",
            "cli",
            &session(true, true),
            &targets,
        ),
        render("https://auth.example", "cli", &session(false, false), &[]),
    ] {
        for line in out.lines() {
            assert_eq!(line, line.trim_end(), "trailing whitespace: {line:?}");
        }
    }
}

/// ⚠️ The trap in `render`: styling before padding embeds ANSI escapes that
/// `len` counts as characters, so coloured rows indent differently.
///
/// ⚠️⚠️ This test needs `set_colors_enabled` to mean anything. `console`
/// disables colour when stderr is not a terminal, which it never is under
/// `cargo test` -- so without forcing it, `apply()` returns plain text, the
/// trap cannot occur, and the test passes against the broken code. Verified by
/// sabotage: styling before padding passed until this line existed.
#[test]
fn padded_width_ignores_colour() {
    console::set_colors_enabled(true);
    // BOTH rows must carry a note: value padding only decides where the note
    // starts, so a fixture with one noted row cannot show the misalignment.
    // My first attempt had exactly that flaw and passed against broken code.
    let targets = vec![target("a", 1, 1), target("b", 22222, 2)];
    let out = render("i", "c", &session(true, true), &targets);
    let offsets: Vec<usize> = out
        .lines()
        .filter(|l| l.contains("changed by you"))
        .map(|l| strip_ansi(l).find("changed by you").expect("present"))
        .collect();
    assert_eq!(
        offsets.len(),
        2,
        "fixture must produce two noted rows:\n{out}"
    );
    assert_eq!(
        offsets[0], offsets[1],
        "note column misaligned once colour is applied:\n{out}"
    );
}

/// Minimal ANSI stripper: enough for the alignment assertion above.
fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Skip to the terminating `m` of the escape sequence.
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
