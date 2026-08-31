//! Shared fixtures for [`super::tests`]. Separate file so both it and the
//! module under test stay under the 200-LoC gate.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use super::{Manifest, digest};

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn tempdir() -> TempDir {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("gauth-managed-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("temp dir");
    TempDir(path)
}

/// A manifest recording that we wrote `entries` into `target`.
pub fn previous(target: &Path, entries: &[(&str, &str)]) -> Manifest {
    let keys: BTreeMap<String, String> = entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), digest(value)))
        .collect();
    let mut targets = BTreeMap::new();
    targets.insert(target.display().to_string(), keys);
    Manifest {
        version: 1,
        targets,
    }
}
