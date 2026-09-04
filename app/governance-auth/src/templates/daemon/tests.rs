//! Mirrors `templates::tests::units`'s coverage for the drain's units --
//! same traps (an unescaped `&` breaks the plist, an un-doubled `%` breaks
//! systemd), same "hand it to the real parser" discipline for the plist.

use super::*;

fn argv() -> Vec<String> {
    [
        "/home/dev/.local/bin/governance-auth",
        "--issuer",
        "https://auth.example/?realm=a&b=100%",
        "serve",
        "--otel",
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .collect()
}

#[test]
fn no_unrendered_template_syntax_survives_in_the_units() {
    for rendered in [
        systemd_service(&argv()).expect("service"),
        launchd_plist("test.label", &argv(), "/tmp/log").expect("plist"),
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
    let service = systemd_service(&argv()).expect("render");
    let line = service
        .lines()
        .find(|line| line.starts_with("ExecStart="))
        .expect("an ExecStart line");
    assert_eq!(
        line,
        "ExecStart=\"/home/dev/.local/bin/governance-auth\" \"--issuer\" \
         \"https://auth.example/?realm=a&b=100%%\" \"serve\" \"--otel\"",
        "each word quoted, and `%` doubled so systemd does not expand it"
    );
}

/// The property that distinguishes this from the drain's unit: it must stay
/// running, not fire once and stop -- falsified by checking for the drain's
/// own shape (`Type=oneshot`, no `Restart=`) rather than only asserting what
/// this unit DOES say, which would pass even if `Type=simple` were dropped
/// and `Type=oneshot` silently crept back in via a copy-paste.
#[test]
fn the_service_is_persistent_not_oneshot() {
    let rendered = systemd_service(&argv()).expect("render");
    assert!(rendered.contains("Type=simple"), "{rendered}");
    assert!(rendered.contains("Restart=on-failure"), "{rendered}");
    assert!(
        !rendered.contains("Type=oneshot"),
        "a persistent daemon must not carry the drain's oneshot shape:\n{rendered}"
    );
    assert!(
        rendered.contains("WantedBy=default.target"),
        "installed via `enable --now` directly, not driven by a `.timer`:\n{rendered}"
    );
}

/// The launchd equivalent of the test above: `KeepAlive`, not
/// `StartInterval`.
#[test]
fn the_plist_keeps_the_process_alive_rather_than_starting_it_on_an_interval() {
    let rendered = launchd_plist("test.label", &argv(), "/tmp/log").expect("render");
    assert!(rendered.contains("<key>KeepAlive</key>"), "{rendered}");
    // The bare word also appears in this template's own explanatory comment
    // ("Not `StartInterval`: ..."), so the key tag is what must be absent,
    // not the substring.
    assert!(
        !rendered.contains("<key>StartInterval</key>"),
        "a persistent daemon must not carry the drain's interval shape:\n{rendered}"
    );
}

/// Handed to the real parser rather than pattern-matched -- see
/// `templates::tests::units::the_plist_parses_as_a_plist` for why.
#[test]
fn the_plist_parses_as_a_plist() {
    let rendered = launchd_plist("test.label", &argv(), "/tmp/log").expect("render");
    let dir = std::env::temp_dir().join(format!("gauth-daemon-plist-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("daemon.plist");
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
        "the ampersand must be escaped and the percent must NOT be doubled here:\n{rendered}"
    );
}
