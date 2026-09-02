//! Does the schedule on disk still run the command this binary has?
//!
//! ## Why this exists at all
//!
//! `copilot-push` became `copilot push` (see [`crate::cli`]'s module doc for
//! the rule that allowed it). `configure` rewrites the unit and the plist, so
//! a developer who re-runs it is fixed -- but a developer who runs
//! `self update` and nothing else is not, and their symptom is a timer that
//! wakes every five minutes, fails on a clap parse error nobody reads, and
//! leaves a spool growing on disk. From inside VS Code that is
//! indistinguishable from a working install, which is precisely the failure
//! [`super`] was written to remove.
//!
//! So `status` asks this instead of assuming, and names `configure` as the
//! fix.
//!
//! ## Rendered-vs-written, not a search for the old name
//!
//! The check is "would `configure` write this file differently right now?",
//! not "does this file contain the string `copilot-push`". Hunting the old
//! spelling would be knowledge of one rename that goes out of date the moment
//! the next one lands -- and it would miss every other reason the unit is
//! stale: a changed endpoint, a moved spool, a binary that was reinstalled
//! somewhere else. Comparing against what the current code renders catches all
//! of them and needs no memory of what used to be there.
//!
//! ## Three-valued, like everything else here
//!
//! `None` means "not answered" -- no collector configured, nothing installed
//! yet, or a file that could not be read or rendered. [`super::Schedule`]'s
//! `active` is three-valued for the same reason: claiming a schedule is
//! current when the question was never asked is the same class of error as
//! claiming it is stale.

use std::{fs, path::Path};

use super::{Invocation, launchd, macos, systemd};
use crate::config::OauthConfig;

/// `Some(true)` when at least one scheduler file on disk differs from what
/// `configure` would write now, `Some(false)` when they all match, `None` when
/// the question could not be answered.
pub fn stale(home: &Path, config: &OauthConfig) -> Option<bool> {
    let invocation = Invocation::resolve(config).ok().flatten()?;
    let rendered = render(home, &invocation)?;

    let mut answered = false;
    for (path, expected) in rendered {
        // A file that is not there is not stale -- it is missing, which the
        // "not scheduled" branch of the drain row already reports, in red.
        // Answering `true` here as well would put two different fixes on one
        // row for one problem.
        let Ok(found) = fs::read_to_string(&path) else {
            continue;
        };
        answered = true;
        if found != expected {
            return Some(true);
        }
    }
    answered.then_some(false)
}

/// What `configure` would write for this platform, rendered but not written.
fn render(home: &Path, invocation: &Invocation) -> Option<Vec<(std::path::PathBuf, String)>> {
    if macos() {
        launchd::plist(home, invocation).ok().map(|pair| vec![pair])
    } else {
        systemd::units(home, invocation).ok()
    }
}
