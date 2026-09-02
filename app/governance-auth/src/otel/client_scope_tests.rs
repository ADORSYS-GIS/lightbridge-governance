//! One collector per audience, so the OTLP endpoint is per-CLIENT and must
//! never be a machine-global environment variable.
//!
//! ## The incident these pin
//!
//! `otel.ai.camer.digital` and `otel-opencode.ai.camer.digital` are two
//! collectors whose OIDC gates accept **one audience each** --
//! `governance-auth-cli` and `opencode-cli` respectively. Once
//! `shell_exports` put the generic `OTEL_EXPORTER_OTLP_ENDPOINT` into
//! `~/.config/governance-auth/otel.env` (sourced from every rc file), it
//! outranked every other client's own default: `@vymalo/opencode-otel`
//! resolves `env.OTEL_EXPORTER_OTLP_ENDPOINT || opts.endpoint`, so on any
//! machine that had run `governance-auth` OpenCode exported to the Claude
//! Code collector and every span 401'd -- observed 2026-09-02 as
//! `otel_export_failed status=401` against 112 `token verification failed`
//! lines on the receiving side.
//!
//! A generic variable is a machine-wide default, and this endpoint has no
//! machine-wide correct value. Each client is therefore configured in its
//! OWN file -- `~/.claude/settings.json`, `~/.codex/config.toml`, VS Code's
//! `settings.json` -- and the shell carries only what is genuinely global to
//! this machine: who the developer is, and where the gateway lives.

use std::{collections::BTreeMap, fs};

use super::{
    claude_code_env, configure_shell_env, shell_exports,
    tests::{settings, tempdir},
};

/// The defect itself: no `OTEL_*` key may reach the shell, because the shell
/// is shared by every OTLP exporter on the machine and this value is not.
#[test]
fn shell_exports_carry_no_otel_key() {
    let exports = shell_exports(&settings());
    let offenders: Vec<_> = exports
        .iter()
        .map(|(key, _)| *key)
        .filter(|key| key.starts_with("OTEL_"))
        .collect();
    assert!(
        offenders.is_empty(),
        "a client-specific OTLP setting leaked into the machine-global shell: {offenders:?}"
    );
}

/// The other half of the same claim: what the shell keeps is genuinely
/// machine-global. `GOVERNANCE_AUTH_*` is this binary's own configuration and
/// `ANTHROPIC_BASE_URL` names the gateway, which is one per org -- neither is
/// per-client the way the collector is.
#[test]
fn shell_exports_keep_identity_and_gateway() {
    let mut with_gateway = settings();
    with_gateway.gateway_url = Some("https://api.example.com".to_owned());
    let exports: BTreeMap<_, _> = shell_exports(&with_gateway).into_iter().collect();

    assert_eq!(
        exports.get("GOVERNANCE_AUTH_ISSUER").map(String::as_str),
        Some("https://auth.example")
    );
    assert_eq!(
        exports.get("GOVERNANCE_AUTH_CLIENT_ID").map(String::as_str),
        Some("cli")
    );
    assert_eq!(
        exports.get("ANTHROPIC_BASE_URL").map(String::as_str),
        Some("https://api.example.com/anthropic")
    );
}

/// The credential must not survive the removal either. It only ever went into
/// the shell to authenticate the endpoint that is no longer exported there, so
/// a bearer left behind would be a secret on disk buying nothing.
#[test]
fn the_ingest_token_no_longer_reaches_the_shell() {
    let home = tempdir();
    fs::write(home.path().join(".bashrc"), "# mine\n").expect("seed bashrc");

    configure_shell_env(home.path(), &settings()).expect("configure");

    for name in ["otel.env", "otel.fish"] {
        let text = fs::read_to_string(home.path().join(".config/governance-auth").join(name))
            .expect("read shell env file");
        assert!(
            !text.contains("ingest-token"),
            "{name} still carries the OTLP bearer:\n{text}"
        );
    }
}

/// Claude Code keeps the full telemetry set -- removing it from the shell must
/// not remove it from the client that has its own file for it. This is the
/// half of the change that could silently turn Claude Code's telemetry off.
#[test]
fn claude_code_settings_still_carry_the_whole_telemetry_set() {
    let env: BTreeMap<_, _> = claude_code_env(&settings()).into_iter().collect();
    for (key, value) in [
        ("CLAUDE_CODE_ENABLE_TELEMETRY", "1"),
        ("OTEL_METRICS_EXPORTER", "otlp"),
        ("OTEL_LOGS_EXPORTER", "otlp"),
        ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "https://otel.example.com"),
        (
            "OTEL_RESOURCE_ATTRIBUTES",
            "service.name=claude-code,user.id=abc-123",
        ),
        (
            "OTEL_EXPORTER_OTLP_HEADERS",
            "Authorization=Bearer ingest-token",
        ),
    ] {
        assert_eq!(
            env.get(key).map(String::as_str),
            Some(value),
            "settings.json lost {key}, which the shell no longer supplies as a fallback"
        );
    }
}

/// Retraction: a machine configured by an OLDER build has the removed
/// variables sitting in `otel.env` and, for a hand-rolled rc, inside the
/// managed block. The next `configure` must take them back -- otherwise the
/// fix only helps machines that never had the bug.
///
/// Both files are rewritten wholesale (the env file by `write_atomically`, the
/// rc block by marker replacement), so this asserts the outcome rather than a
/// mechanism: nothing named `OTEL_` survives.
#[test]
fn a_previously_written_endpoint_is_retracted_on_the_next_run() {
    let home = tempdir();
    let config_dir = home.path().join(".config/governance-auth");
    fs::create_dir_all(&config_dir).expect("create config dir");

    fs::write(
        config_dir.join("otel.env"),
        "export GOVERNANCE_AUTH_ISSUER='https://auth.example'\n\
         export OTEL_EXPORTER_OTLP_ENDPOINT='https://otel.ai.camer.digital'\n\
         export OTEL_EXPORTER_OTLP_PROTOCOL='http/protobuf'\n\
         export OTEL_EXPORTER_OTLP_HEADERS='Authorization=Bearer stale'\n",
    )
    .expect("seed a previously-written env file");
    fs::write(
        config_dir.join("otel.fish"),
        "set -gx OTEL_EXPORTER_OTLP_ENDPOINT 'https://otel.ai.camer.digital'\n",
    )
    .expect("seed a previously-written fish env file");

    let rc = home.path().join(".zshrc");
    fs::write(
        &rc,
        format!(
            "# mine\n{}\nexport OTEL_EXPORTER_OTLP_ENDPOINT='https://otel.ai.camer.digital'\n{}\n",
            super::BLOCK_BEGIN,
            super::BLOCK_END
        ),
    )
    .expect("seed an rc file carrying the old inline block");

    configure_shell_env(home.path(), &settings()).expect("configure");

    for path in [
        config_dir.join("otel.env"),
        config_dir.join("otel.fish"),
        rc.clone(),
    ] {
        let text = fs::read_to_string(&path).expect("read back");
        assert!(
            !text.contains("OTEL_"),
            "{} still exports the retracted collector:\n{text}",
            path.display()
        );
    }
    assert!(
        fs::read_to_string(&rc).expect("read rc").contains("# mine"),
        "the developer's own lines must survive the retraction"
    );
}
