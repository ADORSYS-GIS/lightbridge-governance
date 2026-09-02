//! `Telemetry::survey` -- what it concludes from a manifest on disk.

use std::collections::BTreeMap;

use crate::{
    dashboard::Telemetry,
    managed::{Manifest, digest, manifest_path, save},
};

fn seed(home: &std::path::Path, keys: &[&str]) {
    let path = manifest_path(home);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let mut target = BTreeMap::new();
    for key in keys {
        target.insert((*key).to_owned(), digest("whatever we wrote"));
    }
    let mut targets = BTreeMap::new();
    targets.insert(
        home.join(".codex/config.toml").display().to_string(),
        target,
    );
    save(
        &path,
        &Manifest {
            version: 1,
            targets,
        },
    )
    .expect("save manifest");
}

fn survey(home: &std::path::Path) -> Telemetry {
    Telemetry::survey(Some(home), &config())
}

fn config() -> crate::config::OauthConfig {
    crate::config::OauthConfig {
        issuer: "https://issuer.example".to_owned(),
        client_id: "client".to_owned(),
        scopes: "openid".to_owned(),
        audience: None,
        otel_endpoint: Some("https://otel.example".to_owned()),
        otel_token: None,
        gateway_url: None,
        profile: crate::profile::Profile::Daemon,
        copilot_spool_path: None,
        otel_headers_debounce_ms: 240_000,
        open_browser: false,
        token_exchange: None,
    }
}

#[test]
fn no_manifest_means_nothing_was_applied() {
    let home = crate::managed::testutil::tempdir();
    let t = survey(home.path());
    assert!(!t.applied, "an endpoint alone is not an applied endpoint");
    assert!(!t.has_static_token);
}

/// The regression this module exists for. `otel_token` is never persisted, so
/// the old `config.otel_token.is_some()` test reported "no OTLP token" on every
/// run after the login that wrote one. The manifest still knows.
#[test]
fn a_written_codex_header_counts_as_a_static_token() {
    let home = crate::managed::testutil::tempdir();
    seed(
        home.path(),
        &[
            "otel.environment",
            "otel.exporter.otlp-http.endpoint",
            "otel.exporter.otlp-http.headers.Authorization",
        ],
    );
    let t = survey(home.path());
    assert!(t.applied);
    assert!(t.has_static_token, "the header is in Codex's config");
}

#[test]
fn claude_codes_env_form_of_the_header_counts_too() {
    let home = crate::managed::testutil::tempdir();
    seed(home.path(), &["env.OTEL_EXPORTER_OTLP_HEADERS"]);
    assert!(survey(home.path()).has_static_token);
}

/// Telemetry applied, but no credential written -- the case Codex silently
/// fails on.
#[test]
fn telemetry_without_a_header_is_applied_but_untokened() {
    let home = crate::managed::testutil::tempdir();
    seed(
        home.path(),
        &["otel.environment", "otel.exporter.otlp-http.endpoint"],
    );
    let t = survey(home.path());
    assert!(t.applied);
    assert!(!t.has_static_token);
}

/// Inference wiring is not telemetry. A gateway-only `configure` must not make
/// the row claim OTLP is set up.
#[test]
fn inference_keys_alone_are_not_telemetry() {
    let home = crate::managed::testutil::tempdir();
    seed(
        home.path(),
        &[
            "model_provider",
            "model_providers.lightbridge.base_url",
            "model_providers.lightbridge.auth.command",
        ],
    );
    let t = survey(home.path());
    assert!(!t.applied, "inference keys are not OTLP keys");
    assert!(!t.has_static_token);
}

#[test]
fn without_a_home_nothing_is_claimed() {
    let t = Telemetry::survey(None, &config());
    assert!(!t.applied);
    assert!(!t.has_static_token);
}

/// The upgrade case. `otel-headers` became `otel headers`, so a
/// `settings.json` written by the previous release carries a helper whose
/// command no longer parses -- Claude Code then exports nothing and reports it
/// as "no telemetry", not as "broken helper". `configure` fixes it; this is
/// how the developer who only ran `self update` finds out they need to.
#[test]
fn a_helper_naming_a_retired_command_is_reported_stale() {
    let home = crate::managed::testutil::tempdir();
    seed_claude_settings(home.path(), "/usr/local/bin/governance-auth otel-headers");
    assert!(
        survey(home.path()).stale,
        "a helper ending in a command this binary no longer has must be stale"
    );
}

/// The suffix is what is compared, never the whole line: the binary's path,
/// the issuer and the client id all differ innocently between the `configure`
/// that wrote the file and the `status` reading it back. Nagging on those
/// would train the reader to ignore the row that matters above.
#[test]
fn a_helper_at_a_different_path_is_not_stale() {
    let home = crate::managed::testutil::tempdir();
    seed_claude_settings(
        home.path(),
        "/somewhere/else/governance-auth --issuer https://other --client-id other otel headers",
    );
    assert!(
        !survey(home.path()).stale,
        "only the command tail is ours to judge"
    );
}

/// Writes a real `settings.json` carrying `helper`, and a manifest that claims
/// we wrote that key -- the pair `stale_wiring` reads.
fn seed_claude_settings(home: &std::path::Path, helper: &str) {
    let settings = home.join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().expect("parent")).expect("mkdir");
    std::fs::write(
        &settings,
        serde_json::json!({ "otelHeadersHelper": helper }).to_string(),
    )
    .expect("write settings.json");

    let mut keys = BTreeMap::new();
    keys.insert("otelHeadersHelper".to_owned(), digest(helper));
    let mut targets = BTreeMap::new();
    targets.insert(settings.display().to_string(), keys);
    save(
        &manifest_path(home),
        &Manifest {
            version: 1,
            targets,
        },
    )
    .expect("save manifest");
}
