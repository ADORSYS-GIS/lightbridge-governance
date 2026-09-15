//! The exact `settings.json` entries this module owns: whichever Copilot
//! telemetry path is active, plus the Lightbridge provider wiring. Split out
//! of [`super`] purely for the LoC ceiling: this is pure data, the module
//! doc's actual reasoning lives with [`super::configure`].

use std::path::Path;

use crate::otel::{OtelSettings, binary_path};

/// One place so the "what do we touch" question, the paste-this-by-hand
/// fallback, and `managed::plan`'s retraction candidate list can never drift
/// apart from what [`super::configure`] actually writes.
///
/// Two groups, with deliberately separate gates:
///
/// * the Copilot telemetry keys, present only while a Copilot path is active
///   ([`Self`]'s two flags), and
/// * the `lightbridge.*` inference keys, present whenever `gateway_url` is
///   set -- *not* gated on the OTLP endpoint, so a machine with a gateway but
///   no collector still gets the Lightbridge provider wired (issue #233).
///
/// Empty only when neither a Copilot path nor a gateway is configured -- see
/// [`super::configure`]'s own guard.
pub fn entries(settings: &OtelSettings) -> Vec<(&'static str, serde_json::Value)> {
    let mut out = if settings.copilot_otlp_direct {
        settings
            .endpoint
            .as_deref()
            .map(otlp_settings)
            .unwrap_or_default()
    } else if settings.copilot_drain_available {
        file_settings(&settings.copilot_spool)
    } else {
        Vec::new()
    };

    // `lightbridge.*` is inference wiring, so it moves with the gateway, not
    // with the collector. `gatewayUrl` is the bare host -- the extension
    // appends `/v1/...` itself (`ide/vscode/src/catalogue.ts`) -- and the
    // trailing slash is trimmed for the same reason `anthropic_base_url` does,
    // so a join can't produce `//v1`. `governanceAuthPath` is the absolute
    // path to this binary: the extension spawns it without a shell, so a bare
    // name would only work when `~/.local/bin` happens to be on VS Code's
    // `PATH` (issue #233).
    if let Some(gateway_url) = settings.gateway_url.as_deref() {
        out.push((
            "lightbridge.gatewayUrl",
            serde_json::Value::String(gateway_url.trim_end_matches('/').to_owned()),
        ));
        out.push((
            "lightbridge.governanceAuthPath",
            serde_json::Value::String(binary_path()),
        ));
    }

    out
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
fn file_settings(spool: &Path) -> Vec<(&'static str, serde_json::Value)> {
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

/// `daemon`'s path (issue #272 AC3): Copilot's own `otlp-http` exporter,
/// pointed directly at the loopback daemon. No `headers` key: the daemon
/// needs no credential, which is the entire reason this path is safe where
/// it wasn't before (see [`super::configure`]'s module doc).
///
/// `outfile` is deliberately absent, for the same reason [`file_settings`]
/// omits `otlpEndpoint`: retraction is what removes whichever key the OTHER
/// profile owned.
fn otlp_settings(endpoint: &str) -> Vec<(&'static str, serde_json::Value)> {
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
pub(super) fn entries_hint(settings: &OtelSettings) -> String {
    entries(settings)
        .into_iter()
        .map(|(key, value)| format!("  \"{key}\": {value},"))
        .collect::<Vec<_>>()
        .join("\n")
}
