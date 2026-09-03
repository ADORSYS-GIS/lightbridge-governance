//! The keys this run owns, per target -- the `now` side of
//! [`super::retract_stale`], and the record [`super::save`] writes.
//!
//! Derived from the same conditionals the writers use rather than by reading
//! the files back: a key merely *present* might be the developer's, and
//! recording it as ours would let a later run delete their work.
//!
//! Only string values are recorded. [`super::Format`]'s documents return
//! strings only -- a digest of a rendered number would depend on formatting --
//! so numeric keys like `log_user_prompt` are never retracted. Accepted: they
//! are few, and the alternative is a digest that changes when nothing did.
//!
//! ## Why "what we own" and "what we carry forward" are one function
//!
//! A client the developer opted out of ([`crate::optout`]) is excluded from the
//! write *and* from the retraction, and its previous entry is carried forward
//! verbatim. Computing those two things in two places is precisely how one of
//! them gets forgotten -- and the forgotten one deletes the keys the flag
//! promised to leave alone. [`plan`] does both or neither, and takes the
//! previous manifest as an argument so there is no way to build the map
//! without it.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use super::{Format, Manifest, digest};
use crate::{
    optout::ClientOptOut,
    otel::{CODEX_PROVIDER_ID, OtelSettings, claude_code_env},
};

/// target path -> dotted key -> digest of the value we wrote.
pub fn plan(
    home: &Path,
    settings: &OtelSettings,
    optout: ClientOptOut,
    previous: &Manifest,
) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (path, owned) in targets(home, settings, optout) {
        let target = path.display().to_string();
        let keys = match owned {
            Owned::Keys(keys) => keys,
            // Untouched this run: whatever we recorded last time stays
            // recorded, so `retract_stale` finds nothing stale and a later run
            // without the flag still knows which keys are ours.
            Owned::CarriedForward => {
                if let Some(entry) = previous.targets.get(&target) {
                    out.insert(target, entry.clone());
                }
                continue;
            }
        };
        if keys.is_empty() || !path.is_file() {
            continue;
        }
        let Some(format) = Format::of(&path) else {
            continue;
        };
        let Ok(document) = format.read(&path) else {
            continue;
        };
        let mut recorded = BTreeMap::new();
        for key in keys {
            if let Some(value) = document.get(&key) {
                recorded.insert(key, digest(&value));
            }
        }
        if !recorded.is_empty() {
            out.insert(target, recorded);
        }
    }
    out
}

/// What this run owns in one target file.
enum Owned {
    /// The keys its writer set on this run.
    Keys(Vec<String>),
    /// Its client was opted out, so the previous record stands unchanged.
    CarriedForward,
}

fn owned(declined: bool, keys: Vec<String>) -> Owned {
    if declined {
        Owned::CarriedForward
    } else {
        Owned::Keys(keys)
    }
}

fn targets(home: &Path, settings: &OtelSettings, optout: ClientOptOut) -> Vec<(PathBuf, Owned)> {
    let telemetry = settings.endpoint.is_some();
    let inference = settings.gateway_url.is_some();

    let mut claude: Vec<String> = Vec::new();
    if inference {
        claude.push("apiKeyHelper".to_owned());
        claude.push("env.ANTHROPIC_BASE_URL".to_owned());
    }
    if settings.headers_helper.is_some() {
        claude.push("otelHeadersHelper".to_owned());
    }
    for (key, _) in claude_code_env(settings) {
        claude.push(format!("env.{key}"));
    }

    let mut codex: Vec<String> = Vec::new();
    if inference {
        codex.push("model_provider".to_owned());
        for leaf in ["name", "base_url", "wire_api"] {
            codex.push(format!("model_providers.{CODEX_PROVIDER_ID}.{leaf}"));
        }
        codex.push(format!("model_providers.{CODEX_PROVIDER_ID}.auth.command"));
    }
    if telemetry {
        codex.push("otel.environment".to_owned());
        for kind in ["exporter", "metrics_exporter"] {
            codex.push(format!("otel.{kind}.otlp-http.endpoint"));
            codex.push(format!("otel.{kind}.otlp-http.protocol"));
            // Matches `configure_codex`'s own condition exactly -- unlike
            // `endpoint`/`protocol`, this key is NOT written every run.
            // Listing it unconditionally on `telemetry` alone (#270 AC4's
            // own verification found this) makes it a false "still owned"
            // whenever `--otel-token` is unset after a run that HAD one:
            // the writer leaves the stale header untouched (merge
            // semantics), `plan` reads it back and records it as ours
            // again, and `retract_stale` never sees it as stale -- a
            // credential that should have been removed lingers forever.
            if settings.token.is_some() {
                codex.push(format!("otel.{kind}.otlp-http.headers.Authorization"));
            }
        }
    }

    // `copilot_drain_available`, not `telemetry`: matches
    // `vscode::configure`'s own gate exactly, for the same reason the
    // Codex `Authorization` key above is gated on `settings.token.is_some()`
    // rather than on `telemetry` alone. `telemetry` is true under `daemon`
    // (the loopback substitute), but nothing drains Copilot's spool there
    // yet (#272) -- listing these keys as owned on `telemetry` alone would
    // make a stale `manual`-profile file-exporter config read as "still
    // ours" forever, so switching to `daemon` would stop writing it but
    // never retract it.
    let vscode: Vec<String> = if settings.copilot_drain_available {
        crate::vscode::settings(&settings.copilot_spool)
            .into_iter()
            .map(|(key, _)| key.to_owned())
            .collect()
    } else {
        Vec::new()
    };

    // VS Code ships under several flavour directories and any subset may exist,
    // so each is its own target rather than one path.
    let mut targets = vec![
        (
            home.join(".claude").join("settings.json"),
            owned(optout.claude, claude),
        ),
        (
            home.join(".codex").join("config.toml"),
            owned(optout.codex, codex),
        ),
    ];
    for flavour in crate::vscode::FLAVOURS {
        targets.push((
            crate::vscode::user_dir(home, flavour).join("settings.json"),
            owned(optout.vscode, vscode.clone()),
        ));
    }
    targets
}
