use std::path::Path;

use fixtures::{
    expiring, otel, session, table, target, unsurveyed_daemon, unsurveyed_drain,
    unsurveyed_otel_spool,
};

use super::{
    style::{ago, short, strip_ansi},
    *,
};

mod daemon;
mod drain;
mod duration;
mod fixtures;
mod hints;
mod otel_spool;
mod spool;
mod spool_held;
mod survey;
mod survey_support;
mod targets;
mod telemetry;

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

/// `render_json` must show the identical set of rows `render` does -- same
/// source data, just a different shape -- so a consumer scripting against
/// `--json` can never see something a human reading the table would not.
#[test]
fn json_output_has_the_same_rows_as_the_table() {
    let targets = vec![target("~/.codex/config.toml", 11, 2)];
    let surveys = Surveys {
        telemetry: &otel(Some("https://otel.example"), true),
        daemon: &unsurveyed_daemon(),
        otel_spool: &unsurveyed_otel_spool(),
        spool: &Spool {
            inner: None,
            last_push_age: None,
            last_discard_age: None,
            held_age: None,
            profile: crate::profile::Profile::Manual,
        },
        drain: &unsurveyed_drain(),
    };
    let out = render_json(
        "https://auth.example",
        "cli",
        &session(true, true),
        &surveys,
        &targets,
    )
    .expect("plain strings always serialise");

    let rows: Vec<serde_json::Value> = serde_json::from_str(&out).expect("valid JSON array");
    let labels: Vec<&str> = rows
        .iter()
        .map(|row| row["label"].as_str().expect("label is a string"))
        .collect();
    assert_eq!(
        labels,
        vec![
            "session",
            "issuer",
            "client",
            "telemetry",
            "daemon",
            "otel spool",
            "copilot spool",
            "copilot drain",
            "~/.codex/config.toml",
        ]
    );

    let session_row = &rows[0];
    assert_eq!(session_row["value"], "fresh, 15m left");
    // Lowercase, not Rust's `Debug` spelling ("Green") -- what a consumer of
    // a JSON API actually expects a status field to look like.
    assert_eq!(session_row["colour"], "green");
    // Present and empty, not absent -- so indexing the field never needs an
    // `Option` on the consumer's side.
    assert_eq!(session_row["note"], "");
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
