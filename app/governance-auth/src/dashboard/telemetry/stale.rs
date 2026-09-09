//! Detects helper commands written by older governance-auth versions.

use std::path::Path;

use crate::{cli, managed};

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
pub(super) fn stale_wiring(home: &Path) -> bool {
    let manifest = managed::load(&managed::manifest_path(home));
    manifest
        .targets
        .iter()
        .filter_map(|(target, keys)| {
            let path = std::path::PathBuf::from(target);
            let format = managed::Format::of(&path)?;
            path.is_file().then_some(())?;
            let document = format.read(&path).ok()?;
            Some(
                keys.keys()
                    .filter_map(move |key| managed_key_is_stale(&document, key)),
            )
        })
        .flatten()
        .any(std::convert::identity)
}

/// Checks command-bearing managed keys. Codex's command and arguments are two
/// fields: the command is an executable path and the argv carries the token
/// subcommand. A managed old-style command with no argv is therefore stale.
fn managed_key_is_stale(document: &managed::Document, key: &str) -> Option<bool> {
    match key {
        "otelHeadersHelper" => document
            .get(key)
            .map(|value| !value.ends_with(cli::OTEL_HEADERS_TAIL)),
        "apiKeyHelper" => document
            .get(key)
            .map(|value| !value.ends_with(cli::TOKEN_TAIL)),
        key if key.ends_with(".auth.command") => {
            document.get(key)?;
            let args_key = format!("{}.args", key.trim_end_matches(".command"));
            Some(
                document
                    .get(&args_key)
                    .and_then(|value| serde_json::from_str::<Vec<String>>(&value).ok())
                    .is_none_or(|args| !cli::token_args_are_current(&args)),
            )
        }
        // The command-key branch validates this pair once.
        key if key.ends_with(".auth.args") => None,
        _ => None,
    }
}
