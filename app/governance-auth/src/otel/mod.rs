//! Writes the OpenTelemetry export configuration into Claude Code's
//! `settings.json` and Codex's `config.toml`, so pointing either tool at this
//! org's gateway also points its telemetry at this org's collector. Exporting
//! telemetry is the condition for using the endpoints, so this is wired by
//! `login` automatically rather than left as an opt-in step someone can skip.
//!
//! Both files are **merged, never rewritten**: a developer's `settings.json`
//! carries their theme/permissions and `config.toml` carries their project
//! trust levels and hand-written comments. Only the keys this module owns are
//! touched, and writing is tmp-then-rename so a crash mid-write can't leave
//! either tool with an unparseable config (Codex in particular refuses to
//! start on a malformed `config.toml` -- it doesn't degrade, it exits).
//!
//! ## Why the auth header is not the access token
//!
//! Neither tool re-reads its config mid-session, and neither has a
//! credential-helper hook for OTLP headers the way `apiKeyHelper`/
//! `auth.command` exist for the inference call -- `OTEL_EXPORTER_OTLP_HEADERS`
//! and Codex's `otel.exporter.*.headers` are static strings read once at
//! process start. A 300s Keycloak access token written here would export
//! telemetry for five minutes and then 401 silently for the rest of a session.
//! So the OTLP credential has to be long-lived with server-side revocation --
//! the same conclusion RFC-0002 reached for Foundry, for a different reason
//! (there, changing an agent's env vars requires publishing a new agent
//! version). It is supplied out of band via `--otel-token`, not minted here.
//!
//! ## Module structure (issue #176)
//!
//! Split along real seams — one module per target tool plus a shared settings
//! type — so every file stays under 200 LoC:
//!
//! - [`settings`] — [`OtelSettings`], [`Outcome`], [`binary_path`],
//!   [`identity_attributes`]
//! - [`claude`] — Claude Code (`~/.claude/settings.json`)
//! - [`codex`] — Codex (`~/.codex/config.toml`)
//! - [`shell`] — Shell rc files and `~/.config/governance-auth/otel.env`
//! - [`file`] — crash-safe atomic writes

mod claude;
mod codex;
pub(crate) mod file;
mod outcome;
mod rc;
mod settings;
mod shell;

// The test modules below reach these through `super::*` (they were plain
// imports of the pre-split `otel.rs`); the submodules carry their own copies.
use std::path::Path;
#[cfg(test)]
use std::{collections::BTreeMap, fs, path::PathBuf};

pub(crate) use claude::claude_code_env;
pub use claude::configure_claude_code;
pub(crate) use codex::CODEX_PROVIDER_ID;
pub use codex::configure_codex;
pub(crate) use file::write_atomically;
// Re-export the public API so callers reach everything through `crate::otel`.
pub use outcome::Outcome;
#[cfg(test)]
pub(crate) use rc::{BLOCK_BEGIN, BLOCK_END};
pub use settings::{OtelSettings, binary_path, identity_attributes};
pub use shell::configure_shell_env;
#[cfg(test)]
pub(crate) use shell::shell_exports;

// The OTEL contract (fixed loopback port + client URL shape) lives in
// `otel_port` and is re-exported here so the daemon (#268) and the
// configure/managed/status consumers (#270/#271) reach it from `crate::otel`
// without a second copy — see issue #276 AC3. No in-crate consumer exists yet
// (those land with the consumers), hence the `unused_imports` expectation.
#[expect(
    unused_imports,
    reason = "shipped contract for #268's daemon and #270/#271; remove once a consumer exists"
)]
pub use crate::otel_port::{OTEL_LOOPBACK_ENDPOINT, OTEL_PORT};
#[cfg(test)]
use crate::redacted::Redacted;

/// Configures every supported tool found on this machine, except those
/// [`crate::optout`] names. A tool whose config directory is absent is skipped,
/// not created -- creating `~/.codex` for someone who doesn't use Codex would
/// be surprising, and an empty config directory changes how some tools behave
/// on first run.
pub fn configure_all(
    home: &Path,
    settings: &OtelSettings,
    optout: crate::optout::ClientOptOut,
) -> anyhow::Result<Vec<Outcome>> {
    let previous = crate::managed::load(&crate::managed::manifest_path(home));
    let codex_settings = if optout.codex_telemetry_only {
        OtelSettings {
            gateway_url: None,
            ..settings.clone()
        }
    } else {
        settings.clone()
    };

    let mut outcomes = vec![
        if optout.claude {
            Outcome::Declined {
                path: home.join(".claude"),
                flag: "--no-claude",
            }
        } else {
            configure_claude_code(home, settings)?
        },
        if optout.codex {
            Outcome::Declined {
                path: home.join(".codex"),
                flag: "--no-codex",
            }
        } else {
            configure_codex(home, &codex_settings)?
        },
    ];
    if optout.vscode {
        outcomes.push(Outcome::Declined {
            path: crate::vscode::user_dir(home, "Code"),
            flag: "--no-vscode",
        });
    } else {
        outcomes.extend(crate::vscode::configure_or_warn(home, settings));
    }
    outcomes.extend(configure_shell_env(home, settings)?);

    // Retract anything we wrote last time and did not write now, then record
    // what we own for next time. Non-fatal by design: a failure here leaves a
    // stale key, which is what happens today anyway -- it must never undo a
    // successful configure. See `managed`.
    let mut now = crate::managed::plan(home, settings, optout, &previous);
    for entry in crate::managed::retract_stale(&previous, &mut now) {
        eprintln!("Removed (no longer managed): {entry}");
    }
    let manifest = crate::managed::Manifest {
        version: 1,
        targets: now,
    };
    if let Err(error) = crate::managed::save(&crate::managed::manifest_path(home), &manifest) {
        eprintln!("warning: could not record managed keys: {error:#}");
    }

    Ok(outcomes)
}

#[cfg(test)]
mod client_scope_tests;
#[cfg(test)]
mod codex_provider_tests;
#[cfg(test)]
mod codex_telemetry_only_tests;
#[cfg(test)]
mod tests;
