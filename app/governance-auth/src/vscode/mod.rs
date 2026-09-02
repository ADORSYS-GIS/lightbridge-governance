//! GitHub Copilot Chat's telemetry, via each VS Code flavour's
//! `User/settings.json`.
//!
//! Split out of `crate::otel` when the exporter cut over from `otlp-http` to
//! `file` (issue #176 wanted the split anyway; this is the change that made it
//! worth doing). The two CLI writers left in `otel.rs` configure a tool that
//! exports for itself. This one configures a tool that writes to disk and
//! stops, so it is half of a path whose other half is `crate::copilot` and
//! `crate::schedule` -- a different shape, and now a different file.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::otel::{OtelSettings, Outcome, write_atomically};

#[cfg(test)]
mod tests;

/// Every VS Code flavour whose user-settings directory this understands.
/// Insiders and VSCodium keep entirely separate settings trees, so a
/// developer running one of those gets nothing if only stable `Code` is
/// considered -- and the failure is silent, which is the worst kind.
pub const FLAVOURS: [&str; 3] = ["Code", "Code - Insiders", "VSCodium"];

/// GitHub Copilot in VS Code, via each flavour's `User/settings.json`.
/// Setting names are verbatim from
/// <https://code.visualstudio.com/docs/agents/guides/monitoring-agents>.
///
/// ⚠️ **The exporter written here is `file`, not `otlp-http`, and that is the
/// whole point.** Copilot's direct HTTP exporter carries no Authorization
/// header this binary can supply: `github.copilot.chat.otel.headers` exists
/// but is *static*, and `settings.json` is covered by Settings Sync, so
/// authenticating it means syncing a bearer off-machine. The `otlp-http` this
/// used to write had no header at all -- an authenticating collector 401'd
/// every span while the config looked complete. The file exporter has neither
/// problem: Copilot appends to `outfile`, and `copilot push` ships those bytes
/// with a bearer it refreshes itself (`crate::copilot`, `crate::schedule`).
pub fn configure(home: &Path, settings: &OtelSettings) -> Result<Vec<Outcome>> {
    // VS Code Copilot's OTEL surface is telemetry-only -- there is no
    // gateway/inference setting this writer could touch instead. With no
    // collector there is nowhere for the drain to push, so turning the file
    // exporter on would spool telemetry to disk for ever and export none of
    // it: a quiet no-op is the honest outcome, not a `Skipped` about a tool
    // that may not even be installed here.
    if settings.endpoint.is_none() {
        return Ok(Vec::new());
    }

    let mut outcomes = Vec::new();
    for flavour in FLAVOURS {
        let dir = user_dir(home, flavour);
        if !dir.is_dir() {
            continue;
        }
        outcomes.push(configure_flavour(&dir, &settings.copilot_spool)?);
    }
    if outcomes.is_empty() {
        outcomes.push(Outcome::Skipped(user_dir(home, "Code")));
    }
    Ok(outcomes)
}

/// `~/.config/<flavour>/User` on Linux, `~/Library/Application
/// Support/<flavour>/User` on macOS -- VS Code does not follow
/// `XDG_CONFIG_HOME` on macOS.
pub fn user_dir(home: &Path, flavour: &str) -> PathBuf {
    let base = if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support")
    } else {
        home.join(".config")
    };
    base.join(flavour).join("User")
}

fn configure_flavour(user_dir: &Path, spool: &Path) -> Result<Outcome> {
    let path = user_dir.join("settings.json");

    let existing = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };

    // VS Code's settings.json is JSONC -- comments and trailing commas are
    // legal there and developers really do use them. `serde_json` can't parse
    // that, and the tempting fixes are both destructive: stripping comments
    // to parse then writing plain JSON back deletes them permanently. So a
    // file this can't parse losslessly is REFUSED, with the exact settings
    // printed for the developer to paste. Declining to edit beats silently
    // eating someone's annotated config.
    let mut root: serde_json::Value = if existing.trim().is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(&existing).with_context(|| {
            format!(
                "{} is not plain JSON (VS Code allows JSONC comments/trailing commas, which \
                 cannot be rewritten without discarding them). Leaving it untouched -- add \
                 these settings by hand:\n{}",
                path.display(),
                settings_hint(spool)
            )
        })?
    };

    let object = root
        .as_object_mut()
        .with_context(|| format!("{} is not a JSON object", path.display()))?;

    for (key, value) in settings(spool) {
        object.insert(key.to_owned(), value);
    }

    let mut bytes = serde_json::to_vec_pretty(&root).context("serializing VS Code settings")?;
    bytes.push(b'\n');
    write_atomically(&path, &bytes)?;
    Ok(Outcome::Written(path))
}

/// The exact `settings.json` entries this module owns, in one place so the
/// "what do we touch" question and the paste-this-by-hand fallback can't
/// drift apart.
///
/// `captureContent` is pinned to `false`: the collector's redaction is the
/// authoritative control, but a client that never sends prompts is one fewer
/// place they can leak -- matching the `log_user_prompt = false` choice on
/// the Codex side.
///
/// `otlpEndpoint` is deliberately absent. A key this run does not write is a
/// key `managed::retract_stale` removes on the next `configure`, which is how
/// the cutover from the 401-ing direct exporter cleans up after itself
/// (`vscode_direct_export_is_retracted_on_the_next_configure`).
pub fn settings(spool: &Path) -> Vec<(&'static str, serde_json::Value)> {
    vec![
        (
            "github.copilot.chat.otel.enabled",
            serde_json::Value::Bool(true),
        ),
        (
            "github.copilot.chat.otel.exporterType",
            serde_json::Value::String("file".to_owned()),
        ),
        (
            "github.copilot.chat.otel.outfile",
            serde_json::Value::String(spool.to_string_lossy().into_owned()),
        ),
        (
            "github.copilot.chat.otel.captureContent",
            serde_json::Value::Bool(false),
        ),
    ]
}

/// Rendered into the error when a JSONC `settings.json` can't be rewritten
/// losslessly, so declining to edit still leaves the developer with
/// everything they need to do it themselves.
fn settings_hint(spool: &Path) -> String {
    settings(spool)
        .into_iter()
        .map(|(key, value)| format!("  \"{key}\": {value},"))
        .collect::<Vec<_>>()
        .join("\n")
}
