//! What `status` needs to know about the drain, gathered without a network
//! call.
//!
//! The failure this exists to make visible is a **stopped timer**. A systemd
//! user timer that was never enabled, or a launchd agent that fails on every
//! wake, produces exactly the same observable as everything working: no error
//! anywhere the developer looks. `bytes pending` climbing while `last push`
//! stays put is what distinguishes the two, so both are reported, always --
//! including "never", which is the state a drain that has never once succeeded
//! is in.

use std::path::PathBuf;

use super::checkpoint;
use crate::config::OauthConfig;

pub struct SpoolStatus {
    pub path: PathBuf,
    /// `None` when the spool does not exist -- Copilot creates it on first
    /// export, so this is the normal state before the setting is applied and
    /// must not read as an error.
    pub size: Option<u64>,
    pub offset: u64,
    /// Bytes written but not yet pushed. Saturating, so a spool that shrank
    /// under a stale checkpoint reads 0 rather than underflowing.
    pub pending: u64,
    pub last_push_unix: Option<u64>,
    /// The checkpoint file could not be read. Distinct from "no checkpoint
    /// yet": one is a fresh install, the other is a drain that is failing
    /// every run and would otherwise be indistinguishable from it.
    pub checkpoint_unreadable: bool,
}

impl SpoolStatus {
    /// `None` only when the state directory or the configured path cannot be
    /// resolved at all -- in which case `status` shows no spool row rather
    /// than a row full of guesses.
    pub fn survey(config: &OauthConfig) -> Option<Self> {
        let path = super::resolve_spool_path(config).ok()?;
        let size = std::fs::metadata(&path).ok().map(|metadata| metadata.len());

        let checkpoint_path = checkpoint::path(&crate::cache::state_dir().ok()?);
        let (state, checkpoint_unreadable) = match checkpoint::load(&checkpoint_path) {
            Ok(state) => (state, false),
            Err(_) => (checkpoint::Checkpoint::default(), true),
        };

        Some(Self {
            path,
            size,
            offset: state.offset,
            pending: size.unwrap_or_default().saturating_sub(state.offset),
            last_push_unix: state.last_push_unix,
            checkpoint_unreadable,
        })
    }

    /// Whether the spool is configured at all, from this command's point of
    /// view: a file that has never existed means Copilot's file exporter was
    /// never switched on, which is a different row from "switched on and
    /// stuck".
    pub fn present(&self) -> bool {
        self.size.is_some()
    }
}
