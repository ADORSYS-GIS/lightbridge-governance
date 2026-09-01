//! Column padding and colour for [`super::render`].
//!
//! Split out to keep both halves under the 200-LoC gate, and because the
//! padding/colour ordering rule below is worth reading on its own.

use std::path::Path;

/// Right-pads to `width` counting CHARACTERS, not bytes -- a path with a
/// non-ASCII character would otherwise be padded short.
/// Renders a remaining lifetime for a human.
///
/// ⚠️ `expires_in` goes NEGATIVE once the token is past its `exp`, which is the
/// normal state for a session waiting to be refreshed. Printed raw that reads
/// `needs refresh, -8338s` -- arithmetic, not information. Seen on the test VM;
/// every unit fixture used a positive value, so nothing caught it.
///
/// The plain single-line output keeps the raw seconds: it is a documented
/// surface (`commands.md`) that a test asserts on, and changing it would break
/// anyone parsing it.
pub(super) fn ago(seconds: i64) -> String {
    let past = seconds < 0;
    let text = magnitude(seconds.unsigned_abs());
    if past {
        format!("expired {text} ago")
    } else {
        format!("{text} left")
    }
}

/// Elapsed time, for something that already happened.
///
/// Separate from [`ago`] rather than reusing it with a negated argument, which
/// is what the spool row used to do. Two things were wrong with that. `ago`
/// renders a *remaining lifetime*, so it treats 0 as the future -- a discard
/// that just happened printed "last 0s left". And its past wording is
/// "expired", which is right for a token and wrong for a delivery or a loss:
/// nothing about a discarded record expires.
pub(super) fn since(seconds: u64) -> String {
    if seconds == 0 {
        return "just now".to_owned();
    }
    format!("{} ago", magnitude(seconds))
}

fn magnitude(seconds: u64) -> String {
    match seconds {
        0..=90 => format!("{seconds}s"),
        91..=5400 => format!("{}m", seconds.div_ceil(60)),
        _ => format!("{}h", seconds.div_ceil(3600)),
    }
}

pub(super) fn pad(text: &str, width: usize) -> String {
    let len = text.chars().count();
    format!("{text}{}", " ".repeat(width.saturating_sub(len)))
}

/// The three states worth colouring, kept as our own enum so `render` stays
/// testable without a terminal: `None` is also what a non-TTY gets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Colour {
    None,
    Green,
    Yellow,
    Red,
}

impl Colour {
    pub(super) fn apply(self, text: &str) -> String {
        match self {
            Self::None => text.to_owned(),
            Self::Green => console::style(text).green().to_string(),
            Self::Yellow => console::style(text).yellow().to_string(),
            Self::Red => console::style(text).red().to_string(),
        }
    }
}

/// `~`-relative where possible: the full path is noise in a summary and the
/// home prefix repeats on every row.
///
/// Takes `home` explicitly rather than reading `$HOME`. That keeps it a pure
/// function -- the alternative needed `unsafe { set_var }` to test, which this
/// workspace denies, and a display helper reaching into process state is the
/// wrong shape regardless.
pub(super) fn short(path: &str, home: &Path) -> String {
    let home = home.to_string_lossy();
    match path.strip_prefix(home.as_ref()) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => path.to_owned(),
    }
}
