//! Shell environment: places this binary's own settings, and the gateway URL,
//! in the developer's shell so any terminal (and any subprocess that does not
//! inherit them) can resolve them without flags.

use std::{fs, path::Path};

use anyhow::{Context, Result};

use super::{OtelSettings, Outcome, file::write_atomically, rc};

/// POSIX rc files, then fish (different syntax, different path).
const POSIX_RC_FILES: [&str; 4] = [".bashrc", ".zshrc", ".profile", ".bash_profile"];

/// Every variable placed in the developer's shell, in a stable order.
///
/// ⚠️ **Nothing OTLP goes here, deliberately — see [`configure_shell_env`].**
/// Every client this binary configures has its own file for telemetry, and the
/// generic `OTEL_*` variables are machine-global: one shared value where each
/// client needs a different one.
///
/// What is left is genuinely global to this machine.
/// `GOVERNANCE_AUTH_ISSUER`/`_CLIENT_ID` are here so the binary itself works
/// from any terminal with no flags, which is the other half of what `login`
/// persisting its settings buys (see `config_persist`): the file covers this
/// binary, the environment covers everything that shells out to it.
/// `ANTHROPIC_BASE_URL` names the gateway, of which there is exactly one per
/// org -- it is inference wiring, not telemetry, and Claude Code reads it from
/// `settings.json` anyway (see [`super::claude::claude_code_env`]).
pub(crate) fn shell_exports(settings: &OtelSettings) -> Vec<(&'static str, String)> {
    let mut exports = vec![
        ("GOVERNANCE_AUTH_ISSUER", settings.issuer.clone()),
        ("GOVERNANCE_AUTH_CLIENT_ID", settings.client_id.clone()),
    ];
    if let Some(base_url) = settings.anthropic_base_url() {
        exports.push(("ANTHROPIC_BASE_URL", base_url));
    }
    exports
}

/// Places this binary's own settings, and the gateway URL, in the developer's
/// shell so any terminal (and any subprocess that does not inherit them) can
/// resolve them without flags.
///
/// ## Why no OTLP configuration is written here
///
/// **One collector per audience, so the endpoint is per-CLIENT.** Each
/// collector's OIDC gate accepts exactly one `aud`: `otel.ai.camer.digital`
/// takes `governance-auth-cli`, `otel-opencode.ai.camer.digital` takes
/// `opencode-cli`. `OTEL_EXPORTER_OTLP_ENDPOINT` (and `_PROTOCOL`,
/// `_HEADERS`, `OTEL_METRICS_EXPORTER`, `OTEL_LOGS_EXPORTER`,
/// `OTEL_RESOURCE_ATTRIBUTES`) are **generic OpenTelemetry variables**: once
/// sourced from an rc file they apply to every OTLP exporter started on that
/// machine, and SDKs read the environment *ahead of* their own configured
/// default. So exporting one client's endpoint machine-wide makes every other
/// client's correct default unreachable -- measured 2026-09-02, when OpenCode
/// (`@vymalo/opencode-otel`, `env.OTEL_EXPORTER_OTLP_ENDPOINT ||
/// opts.endpoint`) silently exported to the Claude Code collector and 401'd on
/// every span. There is no machine-wide correct value, so there is no
/// machine-wide variable.
///
/// Every client is reached through its own file instead, and none of them
/// needs the environment: Claude Code via `~/.claude/settings.json`
/// ([`super::claude::claude_code_env`], which also covers a desktop-icon
/// launch with no shell to inherit from), Codex via `[otel]` in
/// `~/.codex/config.toml` ([`super::codex::configure_codex`] writes
/// `endpoint`, `protocol` and `headers` -- its
/// `OtelExporterKind::OtlpHttp` takes `endpoint` as a required field, so the
/// file is authoritative), and VS Code Copilot via its `file` exporter
/// ([`crate::vscode`]) drained out of band.
///
/// ## Why the file is still 0600 and still indirected
///
/// It no longer carries a credential -- that removal is the point -- but `.bashrc`
/// is routinely mode 0644 and routinely committed to a dotfiles repo, and the
/// rc block being a one-line `source` of `~/.config/governance-auth/otel.env`
/// is what keeps it that way for whatever lands here next.
pub fn configure_shell_env(home: &Path, settings: &OtelSettings) -> Result<Vec<Outcome>> {
    let exports = shell_exports(settings);
    if exports.is_empty() {
        // Nothing to place, and an rc block that exports nothing is just noise
        // in someone's shell startup.
        return Ok(Vec::new());
    }

    let env_dir = home.join(".config").join("governance-auth");
    fs::create_dir_all(&env_dir).with_context(|| format!("creating {}", env_dir.display()))?;

    let posix_env = env_dir.join("otel.env");
    let posix =
        crate::templates::shell_env_sh(&exports).context("rendering the POSIX shell env file")?;
    write_atomically(&posix_env, posix.as_bytes())?;

    let fish_env = env_dir.join("otel.fish");
    let fish =
        crate::templates::shell_env_fish(&exports).context("rendering the fish shell env file")?;
    write_atomically(&fish_env, fish.as_bytes())?;

    let mut outcomes = vec![
        Outcome::Written(posix_env.clone()),
        Outcome::Written(fish_env.clone()),
    ];

    for rc in POSIX_RC_FILES {
        let path = home.join(rc);
        // Only existing rc files are edited. Creating a `.zshrc` for someone
        // who doesn't run zsh changes which startup path their shell takes.
        if !path.is_file() {
            continue;
        }
        let line = format!(
            "[ -f \"{}\" ] && . \"{}\"",
            rc::display_with_home(&posix_env),
            rc::display_with_home(&posix_env)
        );
        rc::upsert_block(&path, &line)?;
        outcomes.push(Outcome::Written(path));
    }

    let fish_rc = home.join(".config").join("fish").join("config.fish");
    if fish_rc.is_file() {
        let line = format!(
            "test -f \"{}\"; and source \"{}\"",
            rc::display_with_home(&fish_env),
            rc::display_with_home(&fish_env)
        );
        rc::upsert_block(&fish_rc, &line)?;
        outcomes.push(Outcome::Written(fish_rc));
    }

    Ok(outcomes)
}
