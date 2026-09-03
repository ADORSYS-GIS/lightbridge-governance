//! Atomic tmp-then-rename persistence for a small JSON state file, generic
//! over the value it holds.
//!
//! ## Why this exists as its own module
//!
//! [`crate::copilot::checkpoint`] wrote this exact pattern first: a reader
//! must never see a half-written value, so every store writes to `<path>.tmp`
//! and renames over the real path, and a missing file means "never written"
//! while an unreadable one is fatal (see the module doc there for why
//! defaulting on a parse error is unsafe either direction). `otel_daemon`'s
//! durable spool (#269) needs the identical guarantee for a differently-shaped
//! checkpoint -- the daemon is always-on and moves one record at a time, not a
//! periodic multi-signal wake, so its `Checkpoint` carries none of
//! `copilot::checkpoint::Checkpoint`'s `last_push_records` / `held_since_unix`
//! / per-signal-offset fields. Forcing the two shapes together would mean
//! every reader of one carries fields that mean nothing to it. So the shape
//! stays separate; only the write/read discipline is shared, here.
//!
//! `crate::copilot::checkpoint::{load, store}` are unchanged in behaviour --
//! they now call through to this module rather than duplicating its body.

use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::{Serialize, de::DeserializeOwned};

use crate::copilot::private_file;

/// A missing file means "never written", which is the honest starting state
/// for a checkpoint. An unreadable one is an `Err` -- see the module doc on
/// why neither `T::default()` nor any other silent fallback is safe here.
pub fn load<T: Default + DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "parsing the state file at {}. Delete it to restart from the beginning (which may \
             re-send or re-derive records already delivered) -- deleting it is a decision for \
             whoever is looking, not for this process.",
            path.display()
        )
    })
}

/// Writes tmp-then-rename so a reader never sees a half-written value, and an
/// interrupted write leaves the previous state intact rather than a truncated
/// file the next `load` would refuse to parse.
pub fn store<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let dir = path
        .parent()
        .context("the state path has no parent directory")?;
    private_file::create_dir(dir)?;

    let bytes = serde_json::to_vec(value).context("serialising state")?;
    let tmp = path.with_extension("json.tmp");
    private_file::write(&tmp, &bytes)?;
    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde::Deserialize;

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default();
            let path = std::env::temp_dir().join(format!(
                "durable-state-{tag}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("creating the scratch directory");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Sample {
        n: u64,
    }

    #[test]
    fn missing_file_loads_as_default() {
        let dir = TempDir::new("missing");
        let path = dir.0.join("state.json");
        let loaded: Sample = load(&path).expect("a missing file is not an error");
        assert_eq!(loaded, Sample::default());
    }

    #[test]
    fn store_then_load_round_trips() {
        let dir = TempDir::new("roundtrip");
        let path = dir.0.join("state.json");
        let value = Sample { n: 42 };
        store(&path, &value).expect("store");
        let loaded: Sample = load(&path).expect("load");
        assert_eq!(loaded, value);
    }

    #[test]
    fn an_unparseable_file_is_fatal_not_defaulted() {
        let dir = TempDir::new("garbage");
        let path = dir.0.join("state.json");
        fs::write(&path, b"not json").expect("write garbage");
        let error = load::<Sample>(&path).expect_err("garbage must not silently become default");
        assert!(
            format!("{error:#}").contains("parsing the state file"),
            "names the condition: {error:#}"
        );
    }

    #[test]
    fn store_leaves_no_tmp_file_behind() {
        let dir = TempDir::new("no-tmp");
        let path = dir.0.join("state.json");
        store(&path, &Sample { n: 1 }).expect("store");
        assert!(!path.with_extension("json.tmp").exists());
    }
}
