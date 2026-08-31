//! The telemetry row: is OTLP export actually going to work?
//!
//! ## Why this reads the manifest and not the resolved config
//!
//! `otel_token` is deliberately never persisted to the config file -- it is the
//! OTLP ingest bearer, and `config_persist` refuses to round-trip it. So
//! `config.otel_token.is_some()` is `false` on every run *after* the login that
//! wrote it, and a row keyed off it would report "no OTLP token" on a machine
//! where the token is sitting in Codex's config working fine.
//!
//! The manifest records a key only when that key was actually found in the
//! target file (`otel.rs`, where `recorded.insert` is gated on
//! `document.get(&key)`), so "we manage an OTLP `Authorization` key" is
//! evidence the header really was written. That is what gets reported.
//!
//! Nothing here touches the network. `status` earns its keep by answering fast
//! when something is already wrong; a reachability probe would hang behind the
//! very collector the developer is asking about.

use std::path::Path;

use super::{Session, style::Colour};
use crate::managed;

/// What is configured for telemetry, and whether it was applied.
pub struct Telemetry {
    /// The collector, from the resolved config. Persisted, so it survives.
    pub endpoint: Option<String>,
    /// Whether any OTLP key is currently managed in any target file. Distinct
    /// from `endpoint`: a config file can name a collector that no `login` has
    /// yet written into Claude Code or Codex, and "configured" and "in effect"
    /// are different answers to the question being asked.
    pub applied: bool,
    /// Whether a static OTLP bearer was written. Claude Code refreshes its own
    /// via `otelHeadersHelper`; Codex and VS Code cannot, so for those two this
    /// is the difference between exporting and being rejected.
    pub has_static_token: bool,
}

/// Manifest keys whose presence means a static `Authorization` header was
/// written. Codex nests it (`otel.*.otlp-http.headers.Authorization`); Claude
/// Code carries the env form (`env.OTEL_EXPORTER_OTLP_HEADERS`).
fn is_token_key(key: &str) -> bool {
    key.ends_with(".headers.Authorization") || key.ends_with("OTEL_EXPORTER_OTLP_HEADERS")
}

/// Every OTLP key this binary writes mentions `otel` somewhere in its name --
/// `otel.environment`, `env.OTEL_EXPORTER_OTLP_ENDPOINT`, `otelHeadersHelper`,
/// `github.copilot.chat.otel.enabled`. Inference keys (`apiKeyHelper`,
/// `model_providers.*`) do not, which is the distinction being drawn.
fn is_otel_key(key: &str) -> bool {
    key.to_ascii_lowercase().contains("otel")
}

impl Telemetry {
    /// `home` is `None` when the environment has no usable `HOME`, in which
    /// case nothing can be read and the honest answer is "not applied".
    pub fn survey(home: Option<&Path>, endpoint: Option<String>) -> Self {
        let keys: Vec<String> = home
            .map(|home| managed::load(&managed::manifest_path(home)))
            .map(|manifest| {
                manifest
                    .targets
                    .into_values()
                    .flat_map(std::collections::BTreeMap::into_keys)
                    .collect()
            })
            .unwrap_or_default();

        Self {
            endpoint,
            applied: keys.iter().any(|key| is_otel_key(key)),
            has_static_token: keys.iter().any(|key| is_token_key(key)),
        }
    }

    /// The rendered row: value, colour, and a note that must always name a
    /// command that works from here.
    ///
    /// The note is session-aware because `configure` refuses to run without a
    /// cached session ("no cached session for this issuer/client; run
    /// `governance-auth login` first"). Advising it unconditionally is how a
    /// hint becomes a dead end, which is the bug #214 was opened for.
    pub(super) fn row(&self, session: &Session) -> (String, Colour, String) {
        let command = if session.cached { "configure" } else { "login" };
        match (&self.endpoint, self.applied, self.has_static_token) {
            (None, _, _) => (
                "not configured".to_owned(),
                Colour::Yellow,
                format!("{command} --otel-endpoint <url>"),
            ),
            // A collector is named but nothing was ever written for it: the
            // developer would otherwise read the endpoint and conclude their
            // tools are exporting.
            (Some(endpoint), false, _) => (
                endpoint.clone(),
                Colour::Yellow,
                format!("configured but not applied yet: run {command}"),
            ),
            (Some(endpoint), true, true) => (endpoint.clone(), Colour::Green, String::new()),
            // Yellow, not red, and worded as a consequence rather than a fault:
            // this is the same condition `apply_telemetry` already warns about,
            // and Claude Code is genuinely unaffected.
            (Some(endpoint), true, false) => (
                endpoint.clone(),
                Colour::Yellow,
                "no OTLP token: Codex and VS Code cannot export".to_owned(),
            ),
        }
    }
}
