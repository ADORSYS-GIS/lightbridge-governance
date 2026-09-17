//! Claude Code: `~/.claude/settings.json`, `env` block. Key names are taken
//! verbatim from the "Administrator Configuration" section of
//! <https://code.claude.com/docs/en/monitoring-usage>.

use std::{fs, path::Path};

use anyhow::{Context, Result};

use super::{OtelSettings, Outcome, file::write_atomically};

/// Claude Code: `~/.claude/settings.json`, `env` block. Key names are taken
/// verbatim from the "Administrator Configuration" section of
/// <https://code.claude.com/docs/en/monitoring-usage>.
///
/// `http/protobuf`, not `grpc`: the collector is reached through a public
/// HTTPS ingress here, and the generic `OTEL_EXPORTER_OTLP_ENDPOINT` with an
/// HTTP protocol is the combination that works through one without per-signal
/// port juggling.
pub fn configure_claude_code(home: &Path, settings: &OtelSettings) -> Result<Outcome> {
    let dir = home.join(".claude");
    if !dir.is_dir() {
        return Ok(Outcome::Skipped(dir));
    }
    let path = dir.join("settings.json");

    let mut root: serde_json::Value = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing existing {}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            serde_json::Value::Object(serde_json::Map::new())
        }
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };

    let object = root
        .as_object_mut()
        .with_context(|| format!("{} is not a JSON object", path.display()))?;

    // `otelHeadersHelper` -- Claude Code re-invokes this on an interval and
    // uses whatever JSON headers it prints, so telemetry auth refreshes
    // itself instead of depending on anyone rotating a long-lived key by
    // hand. This is the one client that can do it; see `headers_value`'s
    // callers for the others.
    if let Some(helper) = &settings.headers_helper {
        object.insert(
            "otelHeadersHelper".to_owned(),
            serde_json::Value::String(helper.clone()),
        );
    }

    // `apiKeyHelper` -- the INFERENCE credential, distinct from the telemetry
    // one above. Only written alongside `ANTHROPIC_BASE_URL`: pointing Claude
    // Code's API key at this gateway's tokens while it still talks to
    // api.anthropic.com would send a Keycloak token to Anthropic, so the two
    // keys move together or not at all.
    if let Some(base_url) = settings.anthropic_base_url() {
        object.insert(
            "apiKeyHelper".to_owned(),
            serde_json::Value::String(settings.token_command.clone()),
        );
        let env = object
            .entry("env")
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .with_context(|| format!("`env` in {} is not a JSON object", path.display()))?;
        env.insert(
            "ANTHROPIC_BASE_URL".to_owned(),
            serde_json::Value::String(base_url),
        );
    }

    let env = object
        .entry("env")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .with_context(|| format!("`env` in {} is not a JSON object", path.display()))?;

    // Remove the static header FIRST when a helper is in play. Only adding
    // keys would leave a stale `OTEL_EXPORTER_OTLP_HEADERS` from an earlier
    // run sitting next to the refreshing helper -- the exact silent failure
    // the helper exists to remove, and one that survives every subsequent
    // `configure`. Observed on a real machine before this line existed.
    if settings.headers_helper.is_some() {
        env.remove("OTEL_EXPORTER_OTLP_HEADERS");
    }

    for (key, value) in claude_code_env(settings) {
        env.insert(key.to_owned(), serde_json::Value::String(value));
    }

    let mut bytes = serde_json::to_vec_pretty(&root).context("serializing settings.json")?;
    bytes.push(b'\n');
    write_atomically(&path, &bytes)?;
    Ok(Outcome::Written(path))
}

/// The exact `env` entries this module owns in `settings.json` -- so "which
/// keys do we touch" has one answer, and the test can assert the full set.
pub(crate) fn claude_code_env(settings: &OtelSettings) -> Vec<(&'static str, String)> {
    let mut entries = vec![
        // `apiKeyHelper` output is cached for FIVE MINUTES by default -- the
        // exact lifetime of a Keycloak access token here, so the cache can
        // hand Claude Code a token that expired moments ago and the request
        // 401s. Claude Code re-runs the helper on a 401, so this self-heals,
        // but only after a failed request; keeping the TTL under the token
        // lifetime avoids the failure instead of recovering from it.
        //
        // Unconditional (not gated on `gateway_url`) to match this key's
        // pre-existing behaviour: harmless when `apiKeyHelper` itself isn't
        // written, and not part of the bug this module fixes (that bug was
        // `apiKeyHelper` never being reached at all when only the OTEL
        // endpoint was unset -- see `oauth::apply_telemetry`).
        (
            "CLAUDE_CODE_API_KEY_HELPER_TTL_MS",
            settings.headers_helper_debounce_ms.to_string(),
        ),
        // This gateway serves model names Claude Code doesn't ship in its
        // built-in list (adorsys-coder, minimax-m3, ...), so without
        // discovery they never appear in the `/model` picker at all.
        //
        // It does NOT silence the "not a model this version recognizes"
        // warning -- checked live, the warning still prints with discovery
        // on, because that one is about the assumed 200k context window and
        // is only fixed by `modelOverrides` or CLAUDE_CODE_MAX_CONTEXT_TOKENS.
        // Setting either would mean hard-coding each gateway model's real
        // window here, which this binary has no way to know and which would
        // silently rot as models change. Left to the values repo, where the
        // model list already lives.
        ("CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY", "1".to_owned()),
    ];

    // Everything below is genuinely telemetry-only: without an OTEL endpoint
    // there is no collector to export to, so none of these keys should be
    // written -- the other half of the bug this module fixes (the first half
    // was `apply_telemetry` bailing out before reaching here; this half is
    // `settings.endpoint` no longer silently being any `String` when absent).
    let Some(endpoint) = &settings.endpoint else {
        return entries;
    };

    entries.push(("CLAUDE_CODE_ENABLE_TELEMETRY", "1".to_owned()));
    entries.push(("OTEL_METRICS_EXPORTER", "otlp".to_owned()));
    entries.push(("OTEL_LOGS_EXPORTER", "otlp".to_owned()));
    entries.push(("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf".to_owned()));
    // Without this, Claude Code's own documented default (`delta`) applies.
    // Confirmed live 2026-09-14: a daemon-side byte-level capture showed
    // every `claude_code.*` Sum metric (cost.usage, token.usage,
    // lines_of_code.count, session.count, code_edit_tool.decision) arriving
    // fully populated with real data points -- and every hop from there
    // (this org's `ai-cli-otel` collector, Alloy's OTLP receiver, Alloy's
    // otelcol.exporter.prometheus) reported clean accept/forward counters,
    // zero refused, zero failed. Yet no `claude_code.*` series ever became
    // queryable in Mimir, under any name or label -- only `target_info`
    // (a resource marker with no temporality concept) survived. Prometheus's
    // data model has no delta concept: a Sum needs `cumulative` temporality
    // to exist as a coherent series over time, and neither this org's
    // collector nor Alloy's config runs a `deltatocumulative` processor to
    // convert one. This is the missing piece, not a pipeline drop.
    entries.push((
        "OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE",
        "cumulative".to_owned(),
    ));
    entries.push(("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint.clone()));
    entries.push((
        "OTEL_RESOURCE_ATTRIBUTES",
        settings.resource_attributes_value(),
    ));
    // Off by default in Claude Code -- see files.md's "Resource attributes".
    entries.push(("OTEL_METRICS_INCLUDE_ENTRYPOINT", "1".to_owned()));

    match (&settings.headers_helper, settings.headers_value()) {
        // The helper wins outright when present: a stale static header
        // sitting alongside a refreshing one is the exact silent-failure
        // mode this whole mechanism exists to remove.
        (Some(_), _) => {
            entries.push((
                "CLAUDE_CODE_OTEL_HEADERS_HELPER_DEBOUNCE_MS",
                settings.headers_helper_debounce_ms.to_string(),
            ));
        }
        (None, Some(headers)) => entries.push(("OTEL_EXPORTER_OTLP_HEADERS", headers)),
        (None, None) => {}
    }
    entries
}
