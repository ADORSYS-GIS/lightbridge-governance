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
use crate::{cli, config::OauthConfig, managed};

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
    /// via `otelHeadersHelper` and VS Code Copilot holds no credential at all
    /// since the file-exporter cutover (`crate::vscode`), so Codex is the only
    /// client for which this is the difference between exporting and being
    /// rejected -- **under `manual`**. Under `daemon` this is `false`
    /// unconditionally by design (`TelemetryWiring::resolve`: no client ever
    /// holds a credential when the daemon mints one per forward), so `row`
    /// must read [`Self::profile`] before treating an absent token as a
    /// problem -- found live (#268/#270/#271 end-to-end run): every daemon
    /// install reported "Codex cannot export" despite Codex genuinely
    /// exporting fine through the loopback daemon.
    pub has_static_token: bool,
    /// Whether a helper command already written into a tool's config names a
    /// command this version no longer has. See [`stale_wiring`].
    pub stale: bool,
    /// Which telemetry profile is active -- see [`Self::has_static_token`]'s
    /// doc for why `row` needs this.
    pub profile: crate::profile::Profile,
}

/// Does a command line we wrote into somebody else's config still end with a
/// command we have?
///
/// `copilot-push` became `copilot push` and `otel-headers` became
/// `otel headers` (the rule that allowed those moves is in [`crate::cli`]'s
/// module doc). `configure` rewrites every file that carries one, so a
/// developer who re-runs it is fixed -- but one who runs `self update` and
/// nothing else keeps a `settings.json` whose `otelHeadersHelper` invokes a
/// subcommand that no longer parses, and Claude Code reports that as no
/// telemetry rather than as a broken helper. This row is where they find out.
///
/// Only the SUFFIX is compared, never the whole rendered line: the binary's
/// path, the issuer and the client id all differ innocently between the
/// `configure` that wrote the file and the `status` reading it back, and none
/// of those differences means the wiring is broken.
fn stale_wiring(home: &Path) -> bool {
    let manifest = managed::load(&managed::manifest_path(home));
    manifest
        .targets
        .iter()
        .filter_map(|(target, keys)| {
            let path = std::path::PathBuf::from(target);
            let format = managed::Format::of(&path)?;
            path.is_file().then_some(())?;
            let document = format.read(&path).ok()?;
            Some(keys.keys().filter_map(move |key| {
                let tail = expected_tail(key)?;
                Some((document.get(key)?, tail))
            }))
        })
        .flatten()
        .any(|(value, tail)| !value.ends_with(tail))
}

/// The command a managed key must end with, or `None` when the key holds
/// something other than one of this binary's own command lines.
fn expected_tail(key: &str) -> Option<&'static str> {
    match key {
        "otelHeadersHelper" => Some(cli::OTEL_HEADERS_TAIL),
        // Claude Code's inference helper, and Codex's
        // `model_providers.<id>.auth.command`. Neither name moved, so these
        // never fire today -- they are here because the next rename should
        // not need this function edited to be caught.
        "apiKeyHelper" => Some(cli::TOKEN_TAIL),
        key if key.ends_with(".auth.command") => Some(cli::TOKEN_TAIL),
        _ => None,
    }
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
    pub fn survey(home: Option<&Path>, config: &OauthConfig) -> Self {
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
            endpoint: config.otel_endpoint.clone(),
            applied: keys.iter().any(|key| is_otel_key(key)),
            has_static_token: keys.iter().any(|key| is_token_key(key)),
            stale: home.is_some_and(stale_wiring),
            profile: config.profile,
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
        // Ahead of every other verdict: a helper that no longer parses exports
        // nothing at all, so reporting the collector as green underneath one
        // would be the most misleading line on this table. Same session-aware
        // command as the branches below -- `configure` refuses without a
        // cached session.
        if let (Some(endpoint), true) = (&self.endpoint, self.stale) {
            return (
                endpoint.clone(),
                Colour::Red,
                format!("wiring was written by an older version: run {command}"),
            );
        }
        // `daemon` never carries a static token, by design -- see
        // `Self::has_static_token`'s doc. Treating that as untokened-and-broken
        // is what a real end-to-end run of #268/#270/#271 found: every daemon
        // install reported "Codex cannot export" despite exporting fine.
        let has_static_token =
            self.has_static_token || self.profile == crate::profile::Profile::Daemon;
        match (&self.endpoint, self.applied, has_static_token) {
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
            // Yellow, not red, and worded as a consequence rather than a
            // fault: this is the same condition `apply_telemetry` already warns
            // about, and it now names ONE client -- Claude Code refreshes its
            // own and Copilot needs none. Naming more would be crying wolf.
            // Reachable only under `manual` (see `has_static_token` above).
            (Some(endpoint), true, false) => (
                endpoint.clone(),
                Colour::Yellow,
                "no OTLP token: Codex cannot export".to_owned(),
            ),
        }
    }
}
