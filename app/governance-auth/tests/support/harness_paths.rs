//! `Harness`'s session/state/legacy-cache path helpers -- split out of
//! `harness.rs` (issue #364) purely to keep that file under its
//! grandfathered LoC ceiling; unrelated to the callback-port/bind change
//! that needed the room.

use std::path::PathBuf;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::Harness;

impl Harness {
    /// The LEGACY session location, pre state/cache split. Kept so the
    /// migration test can seed a session where an older build left one.
    pub fn legacy_cache_dir(&self) -> PathBuf {
        let base = if cfg!(target_os = "macos") {
            self.home.path.join("Library").join("Caches")
        } else {
            self.home.path.join(".cache")
        };
        base.join("governance-auth")
    }

    /// Where the session lives now. Mirrors `cache::state_dir`: a refresh
    /// token is STATE, not cache -- see that module's doc for why the
    /// distinction is load-bearing rather than cosmetic.
    pub fn state_dir(&self) -> PathBuf {
        let base = if cfg!(target_os = "macos") {
            self.home.path.join("Library").join("Application Support")
        } else {
            self.home.path.join(".local").join("state")
        };
        base.join("governance-auth")
    }

    /// The durable spool checkpoint's `discarded_total`, read directly from
    /// the state directory -- there is no `status` surface for it (that is
    /// #271's dashboard, not the daemon's own doc), so tests read what the
    /// daemon itself persisted. `Ok(0)` on no checkpoint yet, the honest
    /// starting state.
    pub fn otel_daemon_discarded_total(&self) -> Result<u64> {
        let path = self.state_dir().join("otel-daemon-checkpoint.json");
        match std::fs::read(&path) {
            Ok(bytes) => {
                let value: serde_json::Value = serde_json::from_slice(&bytes)?;
                Ok(value
                    .get("discarded_total")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error.into()),
        }
    }

    fn session_file_name(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.issuer.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.client_id.as_bytes());
        format!("{}.json", hex::encode(hasher.finalize()))
    }

    pub fn legacy_session_path(&self) -> PathBuf {
        self.legacy_cache_dir().join(self.session_file_name())
    }

    /// Mirrors `cache::cache_key`/`cache::session_path` (private to `src/`,
    /// so re-derived here) so tests can inspect the session file the binary
    /// itself would read and write.
    pub fn session_path(&self) -> PathBuf {
        self.state_dir().join(self.session_file_name())
    }

    /// The lock `FileLock::acquire` uses for this issuer/client pair, so a
    /// test can plant the debris a crashed or disk-full run leaves behind.
    pub fn lock_path(&self) -> PathBuf {
        self.state_dir()
            .join(self.session_file_name().replace(".json", ".lock"))
    }

    pub fn seed_session(&self, session: &serde_json::Value) -> Result<()> {
        let dir = self.state_dir();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating state dir {}", dir.display()))?;
        std::fs::write(self.session_path(), session.to_string()).context("writing seeded session")
    }

    /// Seeds a session at the OLD path, as an older build would have left it.
    pub fn seed_legacy_session(&self, session: &serde_json::Value) -> Result<()> {
        let dir = self.legacy_cache_dir();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating legacy cache dir {}", dir.display()))?;
        std::fs::write(self.legacy_session_path(), session.to_string())
            .context("writing seeded legacy session")
    }
}
