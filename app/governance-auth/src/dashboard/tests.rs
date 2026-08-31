//! Tests for [`super`].

use std::path::Path;

use super::{style::short, *};

mod duration;
mod hints;
mod spool;
mod survey;
mod targets;
mod telemetry;

/// [`render`] with the Copilot spool row fixed at "nothing surveyed", so the
/// tests that predate that row keep asserting on exactly what they did before
/// and never touch `$HOME`. The row's own states are covered in [`spool`].
fn table(
    issuer: &str,
    client_id: &str,
    session: &Session,
    telemetry: &Telemetry,
    targets: &[Target],
) -> String {
    render(
        issuer,
        client_id,
        session,
        telemetry,
        &Spool {
            inner: None,
            last_push_age: None,
            last_discard_age: None,
        },
        targets,
    )
}

fn target(path: &str, managed: usize, edited: usize) -> Target {
    Target {
        path: path.to_owned(),
        managed,
        edited,
    }
}

fn otel(endpoint: Option<&str>, has_static_token: bool) -> Telemetry {
    Telemetry {
        endpoint: endpoint.map(ToOwned::to_owned),
        applied: endpoint.is_some(),
        has_static_token,
    }
}

fn session(cached: bool, fresh: bool) -> Session {
    expiring(cached, fresh, 900)
}

fn expiring(cached: bool, fresh: bool, expires_in: i64) -> Session {
    Session {
        cached,
        fresh,
        expires_in,
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
    let fresh = table(
        "https://auth.example",
        "cli",
        &session(true, true),
        &otel(None, false),
        &[],
    );
    assert!(fresh.contains("fresh"), "{fresh}");
    assert!(fresh.contains("15m left"), "{fresh}");

    let none = table(
        "https://auth.example",
        "cli",
        &session(false, false),
        &otel(None, false),
        &[],
    );
    assert!(none.contains("no cached session"), "{none}");
}

/// Found by looking at the output, not by reasoning: padding every value left
/// trailing spaces on most rows, which survive copy-paste into an issue.
#[test]
fn no_row_has_trailing_whitespace() {
    let targets = vec![target("~/.codex/config.toml", 11, 2)];
    for out in [
        table(
            "https://auth.example",
            "cli",
            &session(true, true),
            &otel(Some("https://otel.example"), true),
            &targets,
        ),
        table(
            "https://auth.example",
            "cli",
            &session(false, false),
            &otel(None, false),
            &[],
        ),
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
    let out = table("i", "c", &session(true, true), &otel(None, false), &targets);
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
