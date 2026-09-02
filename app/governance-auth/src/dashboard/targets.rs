//! Per-tool drift: how many keys we manage in each config file, and how many
//! of those the developer has since changed.

use std::path::Path;

use super::{
    Session,
    style::{Colour, short},
};
use crate::managed::{self, Format};

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

/// Appends one row per configured target, or a single "nothing yet" row
/// naming the next command to run. Moved out of `render` itself to keep
/// `mod.rs` under the 200-line ceiling -- this section owns the "no targets
/// yet" wording because it is the one place both the emptiness and the
/// per-target shape are already in scope together.
pub fn rows(
    rows: &mut Vec<(String, String, Colour, String)>,
    targets: &[Target],
    session: &Session,
) {
    if targets.is_empty() {
        rows.push((
            "configured".to_owned(),
            "nothing yet".to_owned(),
            Colour::Yellow,
            // ⚠️ Must name a command that RUNS. This said
            // "run `governance-auth configure`", and bare `configure` exits
            // with "nothing to configure: supply --otel-endpoint and/or
            // --gateway-url" -- so the dashboard sent a first-time user
            // straight into an error. Reported from a real install.
            //
            // The flags were fixed then; the COMMAND still wasn't. `configure`
            // also refuses without a cached session ("no cached session for
            // this issuer/client; run `governance-auth login` first"), which is
            // precisely the state this row appears in on a first run. Same
            // session-aware choice as the telemetry row.
            format!(
                "{} --gateway-url <url> --otel-endpoint <url>",
                if session.cached { "configure" } else { "login" }
            ),
        ));
        return;
    }
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
