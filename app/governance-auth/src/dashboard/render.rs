//! Turning [`super::Surveys`] into rows -- the table, the plain-data form,
//! and the JSON form. Split out of `dashboard`'s own module (200-LoC gate)
//! once `--json` and the `otel spool` row pushed it over; the module doc
//! there still explains why these three forms exist and skip the TTY gate.

use anyhow::{Context, Result};

use super::{
    Session, Surveys, Target,
    style::{Colour, ago, pad},
    targets,
};

/// The rows both [`render`] and [`render_json`] show -- built once so the
/// human table and the machine-readable form can never disagree about which
/// sources exist or what each one says. Neither caller re-derives a row from
/// its own copy of this logic; both take exactly this `Vec`.
fn build_rows(
    issuer: &str,
    client_id: &str,
    session: &Session,
    surveys: &Surveys<'_>,
    targets: &[Target],
) -> Vec<(String, String, Colour, String)> {
    let Surveys {
        telemetry,
        daemon,
        otel_spool,
        spool,
        drain,
    } = surveys;
    let (state, colour) = match (session.cached, session.fresh) {
        (false, _) => ("no cached session".to_owned(), Colour::Red),
        (true, true) => (format!("fresh, {}", ago(session.expires_in)), Colour::Green),
        // Not red: a stale access token is the normal steady state between
        // refreshes, and `token` renews it silently. Flagging it as a problem
        // would train the reader to ignore this line.
        (true, false) => (
            format!("needs refresh, {}", ago(session.expires_in)),
            Colour::Yellow,
        ),
    };

    let mut rows: Vec<(String, String, Colour, String)> = vec![
        ("session".to_owned(), state, colour, String::new()),
        (
            "issuer".to_owned(),
            issuer.to_owned(),
            Colour::None,
            String::new(),
        ),
        (
            "client".to_owned(),
            client_id.to_owned(),
            Colour::None,
            String::new(),
        ),
    ];

    // Telemetry sits with the identity rows, not with the per-file rows: it is
    // configuration state, not something we manage inside someone's file.
    let (value, colour, note) = telemetry.row(session);
    rows.push(("telemetry".to_owned(), value, colour, note));

    // Directly under telemetry: under `daemon` this is the row that answers
    // "is anything actually forwarding what was just configured?" -- see
    // `daemon`'s module doc for why a dead daemon is worse than a dead drain.
    let (value, colour, note) = daemon.row();
    rows.push(("daemon".to_owned(), value, colour, note));

    // Directly under the daemon's own liveness: is its OUTBOUND leg actually
    // keeping up, not just the process being alive? See `otel_spool`'s module
    // doc for the incident (this exact daemon, held on a refused record with
    // nothing in `status` saying so) this row exists to make visible.
    let (value, colour, note) = otel_spool.row();
    rows.push(("otel spool".to_owned(), value, colour, note));

    // Directly under that: the Copilot drain is the one export path whose
    // schedule this binary does not own, so it is the one that can silently
    // stop. See `spool`'s module doc.
    let (value, colour, note) = spool.row();
    rows.push(("copilot spool".to_owned(), value, colour, note));

    // And under that, the schedule that empties it. `configure` installs it
    // now, so a stopped timer is ours to report -- see `drain`.
    let (value, colour, note) = drain.row();
    rows.push(("copilot drain".to_owned(), value, colour, note));

    targets::rows(&mut rows, targets, session);
    rows
}

/// The table, for a human. Returns a `String` rather than printing so it can be
/// asserted on without a terminal.
pub fn render(
    issuer: &str,
    client_id: &str,
    session: &Session,
    surveys: &Surveys<'_>,
    targets: &[Target],
) -> String {
    let rows = build_rows(issuer, client_id, session, surveys, targets);

    // ⚠️ Pad on the PLAIN text, then colour. Styling first embeds ANSI escapes
    // that `str::len` counts as characters, so every coloured row would be
    // indented differently -- invisible in a test that strips colour, obvious
    // to the reader. `padded_width_ignores_colour` pins it.
    let label_width = rows
        .iter()
        .map(|(l, ..)| l.chars().count())
        .max()
        .unwrap_or(0);
    let value_width = rows
        .iter()
        .map(|(_, v, ..)| v.chars().count())
        .max()
        .unwrap_or(0);

    let mut out = String::new();
    for (label, value, colour, note) in rows {
        // Pad the value only when something follows it. Padding every row
        // leaves trailing spaces on most of them, which survive copy-paste and
        // show up as whitespace noise in anything the reader pastes into an
        // issue. `no_row_has_trailing_whitespace` pins it.
        let value = if note.is_empty() {
            colour.apply(&value)
        } else {
            colour.apply(&pad(&value, value_width))
        };
        out.push_str(&format!("  {label:label_width$}   {value}"));
        if !note.is_empty() {
            out.push_str(&format!("   {note}"));
        }
        out.push('\n');
    }
    out
}

/// The same rows as [`render`], as plain owned data -- `colour` already
/// turned into `Colour::as_str`'s string ("none"/"green"/"yellow"/"red"),
/// never the enum itself, so a caller outside this module (`doctor`; see its
/// own module doc) can read a verdict off these rows without this module
/// exposing `Colour`, which stays private to its own rendering.
///
/// [`render_json`] and `doctor::run` both build on exactly this, so neither
/// can see a different set of rows, or a different colour for the same row,
/// than the other.
pub fn rows_plain(
    issuer: &str,
    client_id: &str,
    session: &Session,
    surveys: &Surveys<'_>,
    targets: &[Target],
) -> Vec<(String, String, String, String)> {
    build_rows(issuer, client_id, session, surveys, targets)
        .into_iter()
        .map(|(label, value, colour, note)| (label, value, colour.as_str().to_owned(), note))
        .collect()
}

/// The same rows as [`render`], as one JSON array on stdout -- for anywhere
/// `status` is not attached to a human terminal: a script, CI, or an agent.
/// See the `status` subcommand's `--json` flag and `dashboard`'s own doc for
/// why that path skips the TTY gate. Each element is
/// `{"label", "value", "colour", "note"}`, all strings -- `colour` is one of
/// `"none"`/`"green"`/`"yellow"`/`"red"` (`Colour::as_str`), never Rust's
/// `Debug` spelling; `note` is `""` rather than absent, so a consumer can
/// always index the field without an `Option` on its side.
///
/// Returns `Result` rather than swallowing a serialisation error: every field
/// here is a plain `String`, which cannot itself fail to serialise, but a
/// `Result` costs nothing and keeps this from being the one place that
/// silently prints `"[]"` instead of surfacing a real bug.
pub fn render_json(
    issuer: &str,
    client_id: &str,
    session: &Session,
    surveys: &Surveys<'_>,
    targets: &[Target],
) -> Result<String> {
    #[derive(serde::Serialize)]
    struct Row {
        label: String,
        value: String,
        colour: String,
        note: String,
    }

    let rows = rows_plain(issuer, client_id, session, surveys, targets)
        .into_iter()
        .map(|(label, value, colour, note)| Row {
            label,
            value,
            colour,
            note,
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&rows).context("serialising status as JSON")
}
