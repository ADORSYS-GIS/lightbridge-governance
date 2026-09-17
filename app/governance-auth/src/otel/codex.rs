//! Codex: `~/.codex/config.toml`, `[otel]` table. Key names from
//! <https://learn.chatgpt.com/docs/config-file/config-reference>.
//!
//! Edited through `toml_edit` rather than parse-and-reserialize so the
//! developer's existing comments and key order survive -- this file is
//! hand-maintained.

use std::{fs, path::Path};

use anyhow::{Context, Result};

use super::{OtelSettings, Outcome, binary_path, file::write_atomically};

/// Provider id `governance-auth` owns in `config.toml`. A stable constant so
/// re-running `configure` updates the same block instead of accumulating one
/// per run; any differently-named provider a developer wrote by hand is left
/// strictly alone.
pub(crate) const CODEX_PROVIDER_ID: &str = "governance";

pub fn configure_codex(home: &Path, settings: &OtelSettings) -> Result<Outcome> {
    let dir = home.join(".codex");
    if !dir.is_dir() {
        return Ok(Outcome::Skipped(dir));
    }
    let path = dir.join("config.toml");

    let existing = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };

    let mut document = existing
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parsing existing {}", path.display()))?;

    // `[otel]` is genuinely telemetry-only: without an OTEL endpoint there is
    // no collector to point it at, and the `model_providers` block below
    // (inference) must not depend on it -- that's the bug this branch fixes.
    if let Some(endpoint) = &settings.endpoint {
        let otel = table_entry(document.as_table_mut(), "otel")?;
        otel.insert("environment", toml_edit::value("prod"));
        // Content capture stays off. The collector's own redaction is the
        // authoritative control (RFC-0002 treats that as a release blocker,
        // not an enhancement), but a client that never sends raw prompts in
        // the first place is one fewer place for them to leak.
        otel.insert("log_user_prompt", toml_edit::value(false));

        // `otel.exporter` is a TAGGED ENUM, not a string: the exporter kind is
        // the table NAME and its settings are that table's contents. Writing
        // `exporter = "otlp-http"` with the settings in a sibling table parses
        // as TOML but Codex rejects it at load time with `invalid type: unit
        // variant, expected struct variant in otel.exporter` -- and Codex
        // refuses to start at all on a config it can't load, so getting this
        // wrong bricks the tool rather than just disabling telemetry.
        // Codex uses per-signal URLs verbatim; it does not append the path.
        for (kind, signal) in [("exporter", "logs"), ("metrics_exporter", "metrics")] {
            let exporter = table_entry(otel, kind)?;
            let otlp = table_entry(exporter, "otlp-http")?;
            otlp.insert(
                "endpoint",
                toml_edit::value(format!("{}/v1/{signal}", endpoint.trim_end_matches('/'))),
            );
            otlp.insert("protocol", toml_edit::value("binary"));
            if let Some(token) = &settings.token {
                let headers = table_entry(otlp, "headers")?;
                headers.insert(
                    "Authorization",
                    toml_edit::value(format!("Bearer {}", token.expose())),
                );
            }
        }
    }

    if let Some(base_url) = settings.openai_base_url() {
        // Take over the default. Writing the provider block alone leaves Codex
        // pointed at whatever it used before, so the wiring existed and did
        // nothing -- this key is what selects it. Set here only because
        // `model_providers` is borrowed below; placement in the output is
        // `toml_edit`'s job, see `set_root_scalar`.
        //
        // Deliberately authoritative: it overwrites an existing value rather
        // than deferring to it. Someone who wants another provider for a
        // session has `--config model_provider=...`; someone still talking to
        // api.openai.com while believing they are on the gateway gets no
        // signal at all, and that is the failure this prevents.
        set_root_scalar(
            document.as_table_mut(),
            "model_provider",
            toml_edit::value(CODEX_PROVIDER_ID),
        );

        let providers = table_entry(document.as_table_mut(), "model_providers")?;
        let provider = table_entry(providers, CODEX_PROVIDER_ID)?;
        provider.insert("name", toml_edit::value(CODEX_PROVIDER_ID));
        provider.insert("base_url", toml_edit::value(&base_url));
        // The ONLY value codex-cli 0.146.1 accepts: `wire_api = "chat"` is
        // rejected outright at config load ("no longer supported"), so there
        // is no shape of this block that reaches a chat-completions gateway.
        provider.insert("wire_api", toml_edit::value("responses"));
        provider.decor_mut().set_prefix(
            crate::templates::codex_provider_banner().context("rendering the Codex banner")?,
        );

        let auth = table_entry(provider, "auth")?;
        // Codex passes `command` directly to the OS. Arguments belong in its
        // separate array; putting the whole command line here asks the OS to
        // find one executable whose filename contains every flag and value.
        auth.insert("command", toml_edit::value(binary_path()));
        let args: toml_edit::Array = crate::cli::token_args(&settings.issuer, &settings.client_id)
            .into_iter()
            .collect();
        auth.insert("args", toml_edit::value(args));
        auth.insert(
            "refresh_interval_ms",
            toml_edit::value(i64::try_from(settings.headers_helper_debounce_ms).unwrap_or(240_000)),
        );
    }

    let mut bytes = document.to_string().into_bytes();
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    write_atomically(&path, &bytes)?;
    Ok(Outcome::Written(path))
}

/// Sets a top-level scalar, replacing in place so an existing key keeps its
/// comment (see `config_persist::set` for why `Table::insert` alone loses it).
///
/// A bare TOML key must precede the first table header or it belongs to that
/// table instead -- but `toml_edit` handles this for us: it emits root scalars
/// ahead of tables no matter when they were inserted. Checked by moving this
/// call after `[model_providers]` was built and confirming the output was still
/// a root key, so the call site's ordering is a borrow-checker constraint, not
/// a correctness one. `codex_default_provider_is_a_root_key` pins the result
/// regardless, because it is what Codex actually reads.
fn set_root_scalar(table: &mut toml_edit::Table, key: &str, item: toml_edit::Item) {
    match table.get_mut(key) {
        Some(slot) => *slot = item,
        None => {
            table.insert(key, item);
        }
    }
}

/// `table[key]` on a `toml_edit` table panics when the key exists but holds a
/// non-table (a developer who wrote `otel = "something"` by hand), and
/// `indexing_slicing` is denied in this workspace for exactly that reason.
/// This is the non-panicking equivalent: auto-vivify a table, or report which
/// key is the wrong shape rather than taking the process down.
fn table_entry<'a>(table: &'a mut toml_edit::Table, key: &str) -> Result<&'a mut toml_edit::Table> {
    table
        .entry(key)
        .or_insert(toml_edit::table())
        .as_table_mut()
        .with_context(|| format!("`{key}` already exists in config.toml but is not a table"))
}
