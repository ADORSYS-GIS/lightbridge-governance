//! What the dashboard tells a first-time user to do next. Split from the
//! parent test file to keep both under the 200-LoC gate.

use super::*;

/// ⚠️ Reported from a real install: the hint said "run `governance-auth
/// configure`", and bare `configure` errors. So this asserts the hint carries
/// the flags that make the command RUN, not merely that it says "configure".
#[test]
fn nothing_configured_names_a_command_that_actually_runs() {
    let out = table(
        "https://auth.example",
        "cli",
        &session(true, true),
        &otel(None, false),
        &[],
    );
    assert!(
        out.contains("nothing yet") && out.contains("configure"),
        "{out}"
    );
    assert!(
        out.contains("--gateway-url") || out.contains("--otel-endpoint"),
        "hint lacks the flags `configure` requires -- a dead end:\n{out}"
    );
}

/// The half of that report the flag fix missed: `configure` ALSO refuses
/// without a cached session, and this row is what a first-time user sees before
/// any login. Naming `configure` there is still a dead end.
#[test]
fn with_no_session_the_nothing_configured_hint_is_login() {
    let out = table(
        "https://auth.example",
        "cli",
        &session(false, false),
        &otel(None, false),
        &[],
    );
    let line = strip_ansi(
        out.lines()
            .find(|l| l.contains("nothing yet"))
            .expect("the empty-state row"),
    );
    assert!(line.contains("login --gateway-url"), "{line}");
    assert!(
        !line.contains("configure --"),
        "configure errors without a session: {line}"
    );
}
