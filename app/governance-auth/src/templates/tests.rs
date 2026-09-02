//! Tests for [`super`].

use super::*;

mod units;
fn exports() -> Vec<(&'static str, String)> {
    vec![
        ("GOVERNANCE_AUTH_ISSUER", "https://auth.example".to_owned()),
        (
            "OTEL_RESOURCE_ATTRIBUTES",
            "service.name=ai-cli,user.name=Sinead O'Brien".to_owned(),
        ),
    ]
}

/// The bug this filter exists for. `OTEL_RESOURCE_ATTRIBUTES` carries identity
/// attributes lifted from the access token, so a display name with an
/// apostrophe used to close the shell string early and leave everything after
/// it parsed as code -- in a file sourced by every new shell.
#[test]
fn an_apostrophe_in_a_value_cannot_escape_the_quotes() {
    let rendered = shell_env_sh(&exports()).expect("render");
    assert!(
        rendered.contains(r"'service.name=ai-cli,user.name=Sinead O'\''Brien'"),
        "apostrophe not POSIX-escaped:\n{rendered}"
    );
    // Stronger than any assertion about the text: hand it to a real shell and
    // check the value survives. Counting quotes proves nothing -- `'\''` is
    // three of them, so parity never holds, which is how my first attempt at
    // this test managed to fail on correct output.
    round_trips_through("sh", &rendered);
}

/// Sources `rendered` in `shell` and asserts the value survives byte-for-byte.
fn round_trips_through(shell: &str, rendered: &str) {
    let dir = std::env::temp_dir().join(format!("gauth-tpl-{}-{shell}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("env");
    std::fs::write(&path, rendered).expect("write env file");

    let script = format!(
        ". \"{}\"; printf %s \"$OTEL_RESOURCE_ATTRIBUTES\"",
        path.display()
    );
    let out = std::process::Command::new(shell)
        .arg("-c")
        .arg(&script)
        .output();
    let _ = std::fs::remove_dir_all(&dir);

    let out = match out {
        Ok(out) => out,
        // No such shell here: say so rather than pass silently.
        Err(error) => {
            eprintln!("skipped: cannot run {shell}: {error}");
            return;
        }
    };
    assert!(
        out.status.success(),
        "{shell} failed to source the file: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "service.name=ai-cli,user.name=Sinead O'Brien",
        "value did not survive {shell} quoting"
    );
}

/// fish is not POSIX: it honours `\'` inside single quotes, so the POSIX
/// `'\''` form would be wrong here rather than merely ugly.
#[test]
fn fish_uses_its_own_escape_not_the_posix_one() {
    let rendered = shell_env_fish(&exports()).expect("render");
    assert!(
        rendered.contains(r"'service.name=ai-cli,user.name=Sinead O\'Brien'"),
        "apostrophe not fish-escaped:\n{rendered}"
    );
    assert!(
        !rendered.contains(r"'\''"),
        "POSIX escape leaked into the fish file:\n{rendered}"
    );
    // fish may not be installed; the helper reports a skip rather than passing
    // quietly.
    round_trips_through("fish", &rendered);
}

/// Autoescaping is keyed off a `.html` name and none of these have one. If
/// that ever changes, `&` in a URL becomes `&amp;` and an apostrophe becomes
/// `&#x27;` -- corrupting values silently, in a file nobody reads by eye.
#[test]
fn shell_env_is_not_html_escaped() {
    let with_ampersand = vec![(
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        "https://otel.example/x?a=1&b=2".to_owned(),
    )];
    for rendered in [
        shell_env_sh(&with_ampersand).expect("sh"),
        shell_env_fish(&with_ampersand).expect("fish"),
    ] {
        assert!(
            rendered.contains("a=1&b=2"),
            "ampersand mangled:\n{rendered}"
        );
        assert!(!rendered.contains("&amp;"), "HTML-escaped:\n{rendered}");
        assert!(!rendered.contains("&#x27;"), "HTML-escaped:\n{rendered}");
    }
}

#[test]
fn shell_env_emits_one_export_per_entry_in_order() {
    let rendered = shell_env_sh(&exports()).expect("render");
    let lines: Vec<_> = rendered
        .lines()
        .filter(|l| l.starts_with("export "))
        .collect();
    assert_eq!(lines.len(), 2, "one line per export:\n{rendered}");
    assert!(lines[0].starts_with("export GOVERNANCE_AUTH_ISSUER="));
    assert!(lines[1].starts_with("export OTEL_RESOURCE_ATTRIBUTES="));

    let fish = shell_env_fish(&exports()).expect("render");
    assert_eq!(
        fish.lines().filter(|l| l.starts_with("set -gx ")).count(),
        2
    );
}

/// These land inside TOML that is *merged*, not rewritten, so every line the
/// banner contributes must be a comment. A stray non-comment line would be
/// parsed as a key and could collide with the developer's own.
#[test]
fn toml_banners_are_comments_only() {
    for banner in [
        codex_provider_banner().expect("codex"),
        config_header().expect("header"),
    ] {
        for line in banner.lines() {
            let trimmed = line.trim();
            assert!(
                trimmed.is_empty() || trimmed.starts_with('#'),
                "non-comment line in a TOML banner: {line:?}"
            );
        }
    }
}

/// A template that renders to nothing would leave the file silently
/// un-annotated, and every assertion above about *content* would still pass.
#[test]
fn every_template_produces_something() {
    assert!(!shell_env_sh(&exports()).expect("sh").trim().is_empty());
    assert!(!shell_env_fish(&exports()).expect("fish").trim().is_empty());
    assert!(!codex_provider_banner().expect("codex").trim().is_empty());
    assert!(!config_header().expect("header").trim().is_empty());
}

/// Whitespace control that is invisible until it breaks something. The banner
/// is set as a `toml_edit` prefix, so without a leading newline the first
/// comment glues onto whatever precedes it and the file stops being valid
/// TOML. `codex_auth_command_is_an_absolute_path_not_a_bare_name` caught this
/// as "index not found", which is a long way from the cause.
#[test]
fn codex_banner_starts_on_its_own_line() {
    let banner = codex_provider_banner().expect("render");
    assert!(
        banner.starts_with('\n'),
        "banner must open with a newline or it merges into the previous line: {banner:?}"
    );
    assert!(
        !banner.starts_with("\n\n"),
        "one separating newline, not a gap: {banner:?}"
    );
}

/// The header sits above the first key, so it must end with a blank line.
#[test]
fn config_header_ends_with_a_blank_line() {
    let header = config_header().expect("render");
    assert!(
        header.ends_with("\n\n"),
        "no separation from the first key: {header:?}"
    );
}

#[test]
fn no_unrendered_template_syntax_survives() {
    for rendered in [
        shell_env_sh(&exports()).expect("sh"),
        shell_env_fish(&exports()).expect("fish"),
        codex_provider_banner().expect("codex"),
        config_header().expect("header"),
    ] {
        assert!(
            !rendered.contains("{{") && !rendered.contains("{%") && !rendered.contains("{#"),
            "template syntax leaked into output:\n{rendered}"
        );
    }
}
