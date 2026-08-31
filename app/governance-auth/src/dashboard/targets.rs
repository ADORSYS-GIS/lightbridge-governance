//! Per-tool drift: how many keys we manage in each config file, and how many
//! of those the developer has since changed.

use std::path::Path;

use super::style::short;
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
