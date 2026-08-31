//! A record of which keys this binary wrote into other tools' config files, so
//! it can take one back.
//!
//! `configure` can add and overwrite keys but has no way to **retract** one: a
//! key we stop writing lingers in the developer's config forever. `otel.rs`
//! already carries one hand-written removal for exactly this, added only after
//! it was "observed on a real machine". This makes the next retraction
//! mechanical instead of discovered.
//!
//! ## Why a side manifest and not a marker comment
//!
//! The shell rc writer guards its block with `# >>> governance-auth …` markers,
//! which is the right shape — but JSON has no comments, and VS Code's JSONC
//! file is deliberately *refused* rather than rewritten (a `serde_json`
//! round-trip would delete the developer's own comments). A sentinel key inside
//! their file was the alternative; a manifest keeps two vendors' configs free
//! of a key neither understands. Decided in #210.
//!
//! ## The risk this design carries, and the mitigation
//!
//! A side manifest can drift from reality — the file may be edited, restored
//! from backup, or hand-tuned. Deleting on drift would destroy the developer's
//! own work, which is strictly worse than the stale key we are trying to fix.
//!
//! So a key is removed **only if its current value still hashes to what we
//! recorded writing**. Anything the developer has touched since is left alone.
//! `a_developer_edited_value_is_never_removed` pins that.
//!
//! ## Hashes, never values
//!
//! Codex's block contains `Authorization = "Bearer <token>"`. Recording values
//! would copy that credential into a second file. Only a SHA-256 digest is
//! stored, which answers "is this still what we wrote?" without holding the
//! secret. `the_manifest_never_contains_a_secret` pins it.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod formats;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod testutil;

pub use formats::Format;

/// `<config>/governance-auth/managed.json`.
pub fn manifest_path(home: &Path) -> PathBuf {
    home.join(".config")
        .join("governance-auth")
        .join("managed.json")
}

/// target path -> dotted key -> digest of the value we wrote.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Bumped only on an incompatible shape change. An unknown version is
    /// treated as "no record" rather than an error: refusing to configure
    /// because a bookkeeping file is from the future would be a worse failure
    /// than forgetting what we wrote.
    pub version: u32,
    pub targets: BTreeMap<String, BTreeMap<String, String>>,
}

const VERSION: u32 = 1;

pub fn digest(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
}

/// Reads the manifest. A missing, unreadable or unrecognised file is `default`
/// -- this is bookkeeping, and losing it must never block `configure`.
pub fn load(path: &Path) -> Manifest {
    let Ok(text) = fs::read_to_string(path) else {
        return Manifest::default();
    };
    match serde_json::from_str::<Manifest>(&text) {
        Ok(manifest) if manifest.version == VERSION => manifest,
        _ => Manifest::default(),
    }
}

pub fn save(path: &Path, manifest: &Manifest) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut manifest = manifest.clone();
    manifest.version = VERSION;
    let text = serde_json::to_string_pretty(&manifest).context("serialising the manifest")?;
    fs::write(path, format!("{text}\n")).with_context(|| format!("writing {}", path.display()))
}

/// Removes keys we wrote last time and are not writing now.
///
/// `now` is what this run set, per target. Returns the keys actually removed,
/// so the caller can report them -- a silent deletion from someone else's
/// config file is not something to do quietly.
pub fn retract_stale(
    previous: &Manifest,
    now: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<Vec<String>> {
    let mut removed = Vec::new();

    for (target, old_keys) in &previous.targets {
        let path = PathBuf::from(target);
        let Some(format) = Format::of(&path) else {
            continue;
        };
        // The file is gone, or the tool was uninstalled: nothing to retract,
        // and recreating it to delete a key would be absurd.
        if !path.is_file() {
            continue;
        }
        let keeping = now.get(target);
        let stale: Vec<_> = old_keys
            .iter()
            .filter(|(key, _)| keeping.is_none_or(|k| !k.contains_key(*key)))
            .collect();
        if stale.is_empty() {
            continue;
        }

        let mut document = format.read(&path)?;
        let mut touched = false;
        for (key, recorded) in stale {
            // The mitigation for manifest drift: only remove what still looks
            // like ours. A value the developer has since changed is theirs.
            match document.get(key) {
                Some(current) if &digest(&current) == recorded => {
                    document.remove(key);
                    removed.push(format!("{target}: {key}"));
                    touched = true;
                }
                _ => {}
            }
        }
        if touched {
            format.write(&path, &document)?;
        }
    }

    Ok(removed)
}
