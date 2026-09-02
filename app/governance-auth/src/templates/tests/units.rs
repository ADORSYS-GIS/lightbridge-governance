//! The scheduler unit templates.
//!
//! Separate file because [`super`] is at the LoC ceiling, and because the
//! interesting assertion here is not a substring match: the plist is handed to
//! macOS's own `plutil` to parse. A plist launchd cannot read is not a
//! cosmetic bug -- `launchctl bootstrap` refuses it, so the drain never runs
//! and the only symptom is telemetry that stops arriving.

use super::super::*;

fn argv() -> Vec<String> {
    [
        "/home/dev/.local/bin/governance-auth",
        "--issuer",
        // `&` is a plist parse error unescaped, and a percent sign is a
        // systemd specifier. One fixture, both traps.
        "https://auth.example/?realm=a&b=100%",
        "copilot-push",
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .collect()
}

#[test]
fn no_unrendered_template_syntax_survives_in_the_units() {
    for rendered in [
        systemd_service(&argv(), 240).expect("service"),
        systemd_timer(300).expect("timer"),
        launchd_plist("test.label", &argv(), 300, "/tmp/log").expect("plist"),
    ] {
        assert!(!rendered.trim().is_empty());
        assert!(
            !rendered.contains("{{") && !rendered.contains("{%") && !rendered.contains("{#"),
            "template syntax leaked into output:\n{rendered}"
        );
    }
}

#[test]
fn exec_start_is_one_quoted_word_per_argv_entry() {
    let service = systemd_service(&argv(), 240).expect("render");
    let line = service
        .lines()
        .find(|line| line.starts_with("ExecStart="))
        .expect("an ExecStart line");
    assert_eq!(
        line,
        "ExecStart=\"/home/dev/.local/bin/governance-auth\" \"--issuer\" \
         \"https://auth.example/?realm=a&b=100%%\" \"copilot-push\"",
        "each word quoted, and `%` doubled so systemd does not expand it"
    );
}

/// Handed to the real parser rather than pattern-matched. `plutil` ships with
/// macOS; where it does not exist this says so out loud instead of passing
/// vacuously, because a test that silently runs nothing is a green light for
/// code nobody checked.
#[test]
fn the_plist_parses_as_a_plist() {
    let rendered = launchd_plist("test.label", &argv(), 300, "/tmp/log").expect("render");
    let dir = std::env::temp_dir().join(format!("gauth-plist-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("agent.plist");
    std::fs::write(&path, &rendered).expect("write plist");

    let output = std::process::Command::new("plutil")
        .arg("-lint")
        .arg(&path)
        .output();
    let _ = std::fs::remove_dir_all(&dir);

    match output {
        Ok(output) => assert!(
            output.status.success(),
            "plutil rejected the agent launchd would be asked to load:\n{}\n{rendered}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) => eprintln!("skipped: no plutil on this host ({error})"),
    }
    assert!(
        rendered.contains("<string>https://auth.example/?realm=a&amp;b=100%</string>"),
        "the ampersand must be escaped and the percent must NOT be doubled here -- \
         `%%` is systemd's escape, not launchd's:\n{rendered}"
    );
}
