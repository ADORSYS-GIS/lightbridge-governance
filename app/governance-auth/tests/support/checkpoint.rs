//! Checkpoint helpers shared across `copilot_push_*` integration tests.
//!
//! Extracted from the per-test copies to satisfy #237. The implementations
//! are byte-identical to the originals; only the visibility changed.

use anyhow::{Context, Result};
use serde_json::Value;

use super::harness::Harness;
use super::copilot;

/// Reads and parses the checkpoint file. Returns `None` when the file does
/// not exist yet (a run that never authenticated leaves no checkpoint).
pub fn checkpoint(harness: &Harness) -> Result<Option<Value>> {
    let path = copilot::checkpoint_path(harness);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(
        serde_json::from_slice(&std::fs::read(&path).context("reading the checkpoint")?)
            .context("parsing the checkpoint")?,
    ))
}

/// Reads and parses the checkpoint file, expecting it to exist.
pub fn checkpoint_required(harness: &Harness) -> Result<Value> {
    checkpoint(harness)?.context("expected a checkpoint to exist")
}

/// Extracts a `u64` field from a checkpoint value. Returns `None` when the
/// checkpoint itself is `None` or when the key is absent.
pub fn field(state: &Option<Value>, key: &str) -> Option<u64> {
    state.as_ref()?.get(key)?.as_u64()
}
