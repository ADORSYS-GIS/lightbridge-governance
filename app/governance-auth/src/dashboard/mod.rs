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
//!
//! ## The one exception to the TTY gate
//!
//! `--json` (`render_json`) always gathers and prints all five rows,
//! terminal or not: it is an explicit ask for the machine-readable form, not
//! something a pipe stumbled into. See `render_json`'s own doc.

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
    pub otel_spool: &'a OtelSpool,
    pub spool: &'a Spool,
    pub drain: &'a Drain,
}

#[cfg(test)]
mod tests;

mod daemon;
mod drain;
mod otel_spool;
mod render;
mod spool;
mod status;
mod style;
mod targets;
mod telemetry;
pub use daemon::Daemon;
pub use drain::Drain;
pub use otel_spool::OtelSpool;
pub use render::{render, render_json, rows_plain};
pub use spool::Spool;
pub use status::{status, survey_rows};
pub use targets::{Target, targets};
pub use telemetry::Telemetry;
