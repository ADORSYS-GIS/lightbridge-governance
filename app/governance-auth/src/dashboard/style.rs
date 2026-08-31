//! Column padding and colour for [`super::render`].
//!
//! Split out to keep both halves under the 200-LoC gate, and because the
//! padding/colour ordering rule below is worth reading on its own.

use std::path::Path;

/// Right-pads to `width` counting CHARACTERS, not bytes -- a path with a
/// non-ASCII character would otherwise be padded short.
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
