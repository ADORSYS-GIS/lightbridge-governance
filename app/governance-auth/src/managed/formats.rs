//! Reading and removing a dotted key from the two config formats this binary
//! writes into.
//!
//! Deliberately minimal: get a scalar, remove a key, write back. The manifest
//! only ever needs to ask "is this still what we wrote?" and "take it out
//! again" -- everything else stays with the writers in `otel.rs`, which own the
//! merge semantics.

use std::{fs, path::Path};

use anyhow::{Context, Result};

/// A parsed config file, kept in its native representation so a write-back
/// preserves everything this module did not touch -- TOML comments included.
pub enum Document {
    Json(serde_json::Value),
    Toml(Box<toml_edit::DocumentMut>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Json,
    Toml,
}

impl Format {
    /// By extension, because that is what the writers key off too. An unknown
    /// extension yields `None` and the caller skips the file rather than
    /// guessing at its syntax.
    pub fn of(path: &Path) -> Option<Self> {
        match path.extension().and_then(|e| e.to_str()) {
            Some("json") => Some(Self::Json),
            Some("toml") => Some(Self::Toml),
            _ => None,
        }
    }

    pub fn read(self, path: &Path) -> Result<Document> {
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        match self {
            Self::Json => Ok(Document::Json(
                serde_json::from_str(&text)
                    .with_context(|| format!("parsing {}", path.display()))?,
            )),
            Self::Toml => Ok(Document::Toml(Box::new(
                text.parse()
                    .with_context(|| format!("parsing {}", path.display()))?,
            ))),
        }
    }

    pub fn write(self, path: &Path, document: &Document) -> Result<()> {
        let text = match document {
            Document::Json(value) => {
                let mut text = serde_json::to_string_pretty(value)
                    .with_context(|| format!("serialising {}", path.display()))?;
                text.push('\n');
                text
            }
            Document::Toml(document) => document.to_string(),
        };
        fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }
}

impl Document {
    /// The value at `key`, if it is a string.
    ///
    /// ⚠️ A LITERAL key is tried before treating dots as nesting, and the order
    /// matters. VS Code's `settings.json` uses **flat keys that contain dots**
    /// (`"github.copilot.chat.otel.enabled"` is one key, not five levels),
    /// while Claude Code's `env.OTEL_…` genuinely is nested. Splitting first
    /// would silently never find the VS Code keys -- and "not found" here means
    /// "not ours", so they would never be retracted and the failure would be
    /// invisible. `flat_dotted_keys_are_found_before_nesting` pins it.
    ///
    /// Only strings: a digest of a rendered number or object would depend on
    /// formatting rather than content, making "unchanged since we wrote it"
    /// mean "unchanged AND reserialised identically" -- which would delete keys
    /// it should not.
    pub fn get(&self, key: &str) -> Option<String> {
        match self {
            Self::Json(root) => root
                .get(key)
                .or_else(|| {
                    let mut node = root;
                    for segment in key.split('.') {
                        node = node.get(segment)?;
                    }
                    Some(node)
                })
                .and_then(|node| node.as_str().map(ToOwned::to_owned)),
            Self::Toml(root) => {
                let item = root.as_item();
                item.get(key)
                    .or_else(|| {
                        let mut item = root.as_item();
                        for segment in key.split('.') {
                            item = item.get(segment)?;
                        }
                        Some(item)
                    })
                    .and_then(|item| item.as_str().map(ToOwned::to_owned))
            }
        }
    }

    /// Removes a dotted key. Parent tables are left in place even if they end
    /// up empty: an empty `env` block the developer can see beats this binary
    /// deciding their table is disposable.
    pub fn remove(&mut self, key: &str) {
        // Same literal-first rule as `get`; see its doc comment.
        match self {
            Self::Json(root) => {
                if let Some(object) = root.as_object_mut()
                    && object.contains_key(key)
                {
                    object.remove(key);
                    return;
                }
            }
            Self::Toml(root) => {
                if root.as_table().contains_key(key) {
                    root.as_table_mut().remove(key);
                    return;
                }
            }
        }
        let dotted = key;
        let (parents, leaf) = match dotted.rsplit_once('.') {
            Some((parents, leaf)) => (Some(parents), leaf),
            None => (None, dotted),
        };
        match self {
            Self::Json(root) => {
                let mut node = root;
                if let Some(parents) = parents {
                    for segment in parents.split('.') {
                        let Some(next) = node.get_mut(segment) else {
                            return;
                        };
                        node = next;
                    }
                }
                if let Some(object) = node.as_object_mut() {
                    object.remove(leaf);
                }
            }
            Self::Toml(root) => {
                let mut item = root.as_item_mut();
                if let Some(parents) = parents {
                    for segment in parents.split('.') {
                        let Some(next) = item.get_mut(segment) else {
                            return;
                        };
                        item = next;
                    }
                }
                if let Some(table) = item.as_table_like_mut() {
                    table.remove(leaf);
                }
            }
        }
    }
}
