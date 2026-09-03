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
/// ⚠️ **`manual` writes `file`, not `otlp-http`, and that used to be the whole
/// point.** Copilot's direct HTTP exporter carries no Authorization header
/// this binary can supply under `manual`: `github.copilot.chat.otel.headers`
/// exists but is *static*, and `settings.json` is covered by Settings Sync,
/// so authenticating it there means syncing a bearer off-machine. The file
/// exporter has neither problem under `manual`: Copilot appends to `outfile`,
/// and `copilot push` ships those bytes with a bearer it refreshes itself
/// (`crate::copilot`, `crate::schedule`).
///
/// **`daemon` reintroduces `otlp-http` (#272 AC3), and the reasoning above no
/// longer applies to it**: the loopback daemon needs no credential at all, so
/// there is nothing to sync off-machine by pointing Copilot's own exporter at
/// it directly. `github.copilot.chat.otel.otlpEndpoint` accepting a plain-HTTP
/// loopback address is the one load-bearing assumption this depends on and
/// has **not** been independently confirmed against a real VS Code install --
/// see the epic's (#260) own "settle in week one" note and the manual E2E
/// protocol this ships alongside. If it is false, `daemon` should fall back to
/// the file path here, shrinking this to Codex alone; nothing else in #272
/// depends on it.
pub fn configure(home: &Path, settings: &OtelSettings) -> Result<Vec<Outcome>> {
    // VS Code Copilot's OTEL surface is telemetry-only -- there is no
    // gateway/inference setting this writer could touch instead. With
    // neither Copilot path active, turning either exporter on would either
    // spool telemetry nothing drains (`file`) or point at an endpoint that
    // was never configured (`otlp-http`): a quiet no-op is the honest
    // outcome, not a `Skipped` about a tool that may not even be installed
    // here.
    //
    // Exactly one of the two flags gates each path -- never
    // `settings.endpoint.is_none()`, which is `Some` under `daemon` too (the
    // loopback substitute) regardless of which Copilot path is active.
    // Confirmed live (pre-#272): reading `endpoint` alone here left the file
    // exporter on with the drain that used to empty it removed, and the
    // spool grew unbounded.
    if !settings.copilot_drain_available && !settings.copilot_otlp_direct {
        return Ok(Vec::new());
    }

    let mut outcomes = Vec::new();
    for flavour in FLAVOURS {
        let dir = user_dir(home, flavour);
        if !dir.is_dir() {
            continue;
        }
        outcomes.push(configure_flavour(&dir, settings)?);
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

fn configure_flavour(user_dir: &Path, settings: &OtelSettings) -> Result<Outcome> {
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
                entries_hint(settings)
            )
        })?
    };

    let object = root
        .as_object_mut()
        .with_context(|| format!("{} is not a JSON object", path.display()))?;

    for (key, value) in entries(settings) {
        object.insert(key.to_owned(), value);
    }

    let mut bytes = serde_json::to_vec_pretty(&root).context("serializing VS Code settings")?;
    bytes.push(b'\n');
    write_atomically(&path, &bytes)?;
    Ok(Outcome::Written(path))
}

/// The exact `settings.json` entries this module owns for whichever Copilot
/// path is active, in one place so the "what do we touch" question, the
/// paste-this-by-hand fallback, and `managed::plan`'s retraction candidate
/// list can never drift apart from what this actually writes. Empty when
/// neither path is active -- see [`configure`]'s own guard.
pub fn entries(settings: &OtelSettings) -> Vec<(&'static str, serde_json::Value)> {
    if settings.copilot_otlp_direct {
        settings
            .endpoint
            .as_deref()
            .map(otlp_settings)
            .unwrap_or_default()
    } else if settings.copilot_drain_available {
        file_settings(&settings.copilot_spool)
    } else {
        Vec::new()
    }
}

/// `manual`'s path: Copilot's file exporter, drained out of band by
/// `copilot push`.
///
/// `captureContent` is pinned to `false`: the collector's redaction is the
/// authoritative control, but a client that never sends prompts is one fewer
/// place they can leak -- matching the `log_user_prompt = false` choice on
/// the Codex side.
///
/// `otlpEndpoint` is deliberately absent. A key this run does not write is a
/// key `managed::retract_stale` removes on the next `configure`, which is how
/// switching away from `daemon`'s [`otlp_settings`] cleans up after itself.
pub fn file_settings(spool: &Path) -> Vec<(&'static str, serde_json::Value)> {
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

/// `daemon`'s path (#272 AC3): Copilot's own `otlp-http` exporter, pointed
/// directly at the loopback daemon. No `headers` key: the daemon needs no
/// credential, which is the entire reason this path is safe where it wasn't
/// before (see [`configure`]'s module doc).
///
/// `outfile` is deliberately absent, for the same reason [`file_settings`]
/// omits `otlpEndpoint`: retraction is what removes whichever key the OTHER
/// profile owned.
pub fn otlp_settings(endpoint: &str) -> Vec<(&'static str, serde_json::Value)> {
    vec![
        (
            "github.copilot.chat.otel.enabled",
            serde_json::Value::Bool(true),
        ),
        (
            "github.copilot.chat.otel.exporterType",
            serde_json::Value::String("otlp-http".to_owned()),
        ),
        (
            "github.copilot.chat.otel.otlpEndpoint",
            serde_json::Value::String(endpoint.to_owned()),
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
fn entries_hint(settings: &OtelSettings) -> String {
    entries(settings)
        .into_iter()
        .map(|(key, value)| format!("  \"{key}\": {value},"))
        .collect::<Vec<_>>()
        .join("\n")
}
