//! Shared scratch-`$HOME`/config-file helpers for every sibling module
//! here -- `precedence` was split by topic across files (issue #364's
//! LoC-gate fallout) to keep each one under the loc-gate's 200-line
//! threshold for a new file.

use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

/// Minimal scratch dir, removed on drop -- same hand-rolled pattern
/// used in `otel.rs`'s and `config_file.rs`'s own test modules.
pub(crate) struct TempDir(std::path::PathBuf);

impl TempDir {
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn tempdir() -> TempDir {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "governance-auth-config-precedence-{}-{unique}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create temp dir");
    TempDir(path)
}

/// A path in a freshly-made temp dir that nothing has written to --
/// `config_file::load` treats a missing file as "this layer has
/// nothing to say", not an error, so this stands in for "this layer
/// is absent" throughout.
pub(crate) fn absent_path(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join("absent.toml")
}

pub(crate) fn write_config(dir: &TempDir, name: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, contents).expect("write test config file");
    path
}

/// The minimal always-present layer: issuer/client-id as if parsed
/// from a flag or env var (clap has already merged those two by the
/// time `OauthConfigArgs` exists, so this crate has no way to tell
/// them apart downstream -- see `tests/config_precedence.rs` for the
/// one layer that's actually clap's job).
pub(crate) fn base_args() -> OauthConfigArgs {
    OauthConfigArgs {
        issuer: Some("https://issuer.example/realms/platform".to_owned()),
        client_id: Some("cli".to_owned()),
        scopes: None,
        audience: None,
        otel_endpoint: None,
        otel_token: None,
        gateway_url: None,
        profile: None,
        copilot_spool_path: None,
        otel_headers_debounce_ms: None,
        open_browser: None,
        callback_port: None,
        callback_bind: None,
        token_exchange: None,
        exchange_issuer: None,
        exchange_token_endpoint: None,
        exchange_client_id: None,
        exchange_scopes: None,
    }
}
