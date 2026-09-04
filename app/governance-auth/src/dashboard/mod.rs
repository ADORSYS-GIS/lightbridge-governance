//! The human-facing view of `status`.
//!
//! ## Why this is gated on a TTY, and why the plain output stays
//!
//! `status` prints to **stderr** and its three plain lines are a documented
//! surface (`docs/governance-auth/commands.md`) that at least one test asserts
//! on. So the table below is an *addition for a human at a terminal*, never a
//! replacement: with no TTY, `status` prints exactly what it always did.
//!
//! That is not politeness. This binary's `token` is spawned by Claude Code and
//! Codex every few minutes with nobody watching, its stdout is a parsed
//! contract, and rendering a table into a pipe would be work nobody sees at
//! best and a broken parse at worst. `console::user_attended()` is the switch.
//!
//! ## Where the data comes from
//!
//! Local files, plus one short query to the platform's scheduler. The session
//! is already cached, and the managed-key manifest (`crate::managed`) already
//! records what `configure` wrote -- "edited by you" falls out of the same
//! digest comparison that decides whether a key may be retracted. The one
//! exception is the `copilot drain` row, which asks systemd or launchd whether
//! the timer is running, because no file on disk answers that. Nothing here
//! touches the network: `status` earns its keep by answering fast when
//! something is already wrong.

/// Whether a human is looking. Extracted so tests can render both branches
/// without a terminal.
pub fn attended() -> bool {
    console::user_attended_stderr()
}

pub struct Session {
    pub cached: bool,
    pub fresh: bool,
    pub expires_in: i64,
}

/// The single line `status` has always printed. Unchanged on purpose.
pub fn plain(session: &Session) -> String {
    if !session.cached {
        return "no cached session".to_owned();
    }
    format!(
        "session cached, {}, expires in {}s",
        if session.fresh {
            "fresh"
        } else {
            "needs refresh"
        },
        session.expires_in
    )
}

/// The four per-source surveys `render` turns into rows, grouped into one
/// argument rather than four separate ones: they always travel together
/// (one caller, `status`, and one shape of fixture in tests), and a fifth
/// row added here (this struct exists because a fifth positional argument
/// is what tripped `clippy::too_many_arguments`) belongs in this struct, not
/// in `render`'s own signature again.
pub struct Surveys<'a> {
    pub telemetry: &'a Telemetry,
    pub daemon: &'a Daemon,
    pub spool: &'a Spool,
    pub drain: &'a Drain,
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
    let Surveys {
        telemetry,
        daemon,
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

#[cfg(test)]
mod tests;

mod daemon;
mod drain;
mod spool;
mod status;
mod style;
mod targets;
mod telemetry;
pub use daemon::Daemon;
pub use drain::Drain;
pub use spool::Spool;
pub use status::status;
use style::{Colour, ago, pad};
pub use targets::{Target, targets};
pub use telemetry::Telemetry;
