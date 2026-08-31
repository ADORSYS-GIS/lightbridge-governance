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
//! Nothing new is collected. The session is already cached, and the managed-key
//! manifest (`crate::managed`) already records what `configure` wrote. "Edited
//! by you" falls out of the same digest comparison that decides whether a key
//! may be retracted -- see [`crate::managed`]'s module doc.

use std::path::Path;

use crate::managed::{self, Format};

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

/// One configured tool: how many keys we manage in it, and how many of those
/// the developer has since changed.
pub struct Target {
    pub path: String,
    pub managed: usize,
    pub edited: usize,
}

/// Reads the manifest and reports, per target, how many managed keys are still
/// ours and how many have drifted.
///
/// A file that has been deleted since we wrote it is reported with `managed`
/// intact and `edited` zero rather than being dropped: "the tool is gone" is
/// something the reader should see, not something to hide by omission.
pub fn targets(home: &Path) -> Vec<Target> {
    let manifest = managed::load(&managed::manifest_path(home));
    let mut out = Vec::new();
    for (target, keys) in &manifest.targets {
        let path = Path::new(target);
        let mut edited = 0;
        if let Some(format) = Format::of(path)
            && path.is_file()
            && let Ok(document) = format.read(path)
        {
            for (key, recorded) in keys {
                match document.get(key) {
                    Some(current) if &managed::digest(&current) == recorded => {}
                    // Absent or changed: either way it is no longer the value
                    // we wrote, which is what the reader needs to know.
                    _ => edited += 1,
                }
            }
        }
        out.push(Target {
            // Shortened here, where `home` is already known, so `render` needs
            // no process state at all.
            path: short(target, home),
            managed: keys.len(),
            edited,
        });
    }
    out
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

/// The table, for a human. Returns a `String` rather than printing so it can be
/// asserted on without a terminal.
pub fn render(issuer: &str, client_id: &str, session: &Session, targets: &[Target]) -> String {
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

    if targets.is_empty() {
        rows.push((
            "configured".to_owned(),
            "nothing yet".to_owned(),
            Colour::Yellow,
            "run `governance-auth configure`".to_owned(),
        ));
    } else {
        for target in targets {
            let (note, colour) = if target.edited == 0 {
                (String::new(), Colour::Green)
            } else {
                (
                    format!("{} changed by you, left alone", target.edited),
                    Colour::Yellow,
                )
            };
            rows.push((
                target.path.clone(),
                format!("{} keys managed", target.managed),
                colour,
                note,
            ));
        }
    }

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

mod style;
use style::{Colour, ago, pad, short};
