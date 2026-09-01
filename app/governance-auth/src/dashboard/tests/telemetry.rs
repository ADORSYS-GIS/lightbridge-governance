//! The telemetry row. Split from the parent test file to keep both under the
//! 200-LoC gate.

use super::*;

fn row(out: &str) -> String {
    strip_ansi(
        out.lines()
            .find(|l| l.contains("telemetry"))
            .expect("telemetry row present"),
    )
}

#[test]
fn unconfigured_telemetry_says_how_to_configure_it() {
    let out = table("i", "c", &session(true, true), &otel(None, false), &[]);
    let line = row(&out);
    assert!(line.contains("not configured"), "{line}");
    assert!(
        line.contains("--otel-endpoint"),
        "hint must be runnable: {line}"
    );
}

#[test]
fn a_configured_endpoint_is_shown() {
    let out = table(
        "i",
        "c",
        &session(true, true),
        &otel(Some("https://otel.example"), true),
        &[],
    );
    let line = row(&out);
    assert!(line.contains("https://otel.example"), "{line}");
    assert!(!line.contains("not configured"), "{line}");
}

/// The condition `apply_telemetry` already warns about, surfaced where a human
/// will actually look. Claude Code refreshes its own headers; Codex and VS Code
/// cannot, so without a static token their exports are rejected -- silently.
#[test]
fn an_endpoint_without_a_token_names_who_it_breaks() {
    let out = table(
        "i",
        "c",
        &session(true, true),
        &otel(Some("https://otel.example"), false),
        &[],
    );
    let line = row(&out);
    assert!(line.contains("https://otel.example"), "{line}");
    assert!(line.contains("Codex") && line.contains("VS Code"), "{line}");
    // Claude Code is unaffected; saying otherwise would be wrong and would
    // send someone looking in the wrong place.
    assert!(
        !line.contains("Claude"),
        "Claude Code is unaffected: {line}"
    );
}

/// `status` must not make network calls -- it answers fastest when something is
/// already broken, and a probe would hang behind an unreachable collector. This
/// pins that the row reports CONFIGURATION, never reachability.
#[test]
fn the_row_reports_configuration_not_reachability() {
    let out = table(
        "i",
        "c",
        &session(true, true),
        &otel(Some("https://otel.example"), true),
        &[],
    );
    let line = row(&out).to_lowercase();
    for word in ["reachable", "unreachable", "healthy", "up", "down", "ping"] {
        assert!(
            !line.split_whitespace().any(|w| w.trim_matches(',') == word),
            "implies a probe that never happens: {line}"
        );
    }
}

/// `configure` errors with "no cached session ... run `governance-auth login`
/// first". Advising it to someone who has no session is exactly the dead-end
/// hint #214 was opened for, so the note has to follow the session.
#[test]
fn without_a_session_the_hint_is_login_not_configure() {
    let out = table("i", "c", &session(false, false), &otel(None, false), &[]);
    let line = row(&out);
    assert!(line.contains("login --otel-endpoint"), "{line}");
    // "not configured" contains "configure"; the command form is what matters.
    assert!(
        !line.contains("configure --"),
        "configure would error from here: {line}"
    );
}

#[test]
fn with_a_session_the_hint_is_configure() {
    let out = table("i", "c", &session(true, true), &otel(None, false), &[]);
    assert!(row(&out).contains("configure --otel-endpoint"));
}

/// An endpoint in the config file that no `login` has applied yet: the tools
/// are not exporting, and the row must not imply they are.
#[test]
fn a_configured_but_unapplied_endpoint_says_so() {
    let unapplied = Telemetry {
        endpoint: Some("https://otel.example".to_owned()),
        applied: false,
        has_static_token: false,
    };
    let out = table("i", "c", &session(true, true), &unapplied, &[]);
    let line = row(&out);
    assert!(line.contains("not applied yet"), "{line}");
    assert!(line.contains("configure"), "must name the fix: {line}");
}
