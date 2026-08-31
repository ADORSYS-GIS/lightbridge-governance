//! What the dashboard tells a first-time user to do next. Split from the
//! parent test file to keep both under the 200-LoC gate.

use super::*;

/// ⚠️ Reported from a real install: the hint said "run `governance-auth
/// configure`", and bare `configure` errors. So this asserts the hint carries
/// the flags that make the command RUN, not merely that it says "configure".
#[test]
fn nothing_configured_names_a_command_that_actually_runs() {
    let out = render("https://auth.example", "cli", &session(true, true), &[]);
    assert!(
        out.contains("nothing yet") && out.contains("configure"),
        "{out}"
    );
    assert!(
        out.contains("--gateway-url") || out.contains("--otel-endpoint"),
        "hint lacks the flags `configure` requires -- a dead end:\n{out}"
    );
}
