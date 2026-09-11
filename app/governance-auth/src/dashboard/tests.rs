use std::path::Path;

use super::{
    style::{short, strip_ansi},
    *,
};

mod daemon;
mod drain;
mod duration;
mod hints;
mod otel_spool;
mod spool;
mod spool_held;
mod survey;
mod survey_support;
mod targets;
mod telemetry;

/// [`render`] with the two Copilot rows fixed at "nothing surveyed", so tests
/// predating them assert exactly what they did before and never touch `$HOME`
/// (never running `systemctl` for the drain row). Covered in [`spool`]/[`drain`].
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
        &Surveys {
            telemetry,
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
        },
        targets,
    )
}

/// `home` unresolvable, so [`Drain::row`] takes its "unknown" branch without
/// asking the platform's scheduler anything.
pub(super) fn unsurveyed_drain() -> Drain {
    Drain {
        schedule: None,
        collector: false,
        stale: None,
        profile: crate::profile::Profile::Manual,
    }
}

/// [`Daemon::row`]'s "unknown" branch, for the same reason as
/// [`unsurveyed_drain`] above.
pub(super) fn unsurveyed_daemon() -> Daemon {
    Daemon {
        schedule: None,
        profile: crate::profile::Profile::Daemon,
        collector: true,
    }
}

/// `Profile::Manual` -> [`OtelSpool::row`]'s quiet "not applicable" branch,
/// for the same reason [`unsurveyed_drain`] picks `Manual`: a test that is not
/// specifically about this row should not have to reason about a daemon spool
/// that was never surveyed.
pub(super) fn unsurveyed_otel_spool() -> OtelSpool {
    OtelSpool {
        inner: None,
        worst_quarantined_age: None,
        last_discard_age: None,
        profile: crate::profile::Profile::Manual,
    }
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
        stale: false,
        // `manual`: every existing caller of this helper is asserting on
        // `has_static_token` meaning something, which is only true under
        // `manual` (`Telemetry::row`'s doc) -- a `daemon` fixture belongs in
        // `dashboard/tests/telemetry.rs`'s own daemon-specific test instead.
        profile: crate::profile::Profile::Manual,
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
