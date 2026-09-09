//! Codex telemetry-only configuration invariants.

use super::{
    tests::{settings, settings_with_gateway, tempdir},
    *,
};
use crate::optout::ClientOptOut;

#[test]
fn codex_telemetry_only_does_not_add_a_provider_and_keeps_otel() {
    let home = tempdir();
    fs::create_dir_all(home.path().join(".codex")).expect("codex dir");
    let configured = OtelSettings {
        endpoint: Some("http://127.0.0.1:17457".to_owned()),
        ..settings_with_gateway()
    };
    configure_all(
        home.path(),
        &configured,
        ClientOptOut {
            codex_telemetry_only: true,
            ..ClientOptOut::default()
        },
    )
    .expect("telemetry-only configure");

    let text = fs::read_to_string(home.path().join(".codex/config.toml")).expect("read");
    let document: toml_edit::DocumentMut = text.parse().expect("valid TOML");
    assert!(
        document.get("model_provider").is_none(),
        "default provider was added:\n{text}"
    );
    assert!(
        document.get("model_providers").is_none(),
        "provider table was added:\n{text}"
    );
    assert_eq!(
        document["otel"]["exporter"]["otlp-http"]["endpoint"].as_str(),
        Some("http://127.0.0.1:17457")
    );
}

#[test]
fn codex_telemetry_only_leaves_an_existing_managed_provider_unchanged() {
    let home = tempdir();
    fs::create_dir_all(home.path().join(".codex")).expect("codex dir");
    let configured = OtelSettings {
        endpoint: Some("http://127.0.0.1:17457".to_owned()),
        ..settings_with_gateway()
    };
    configure_all(home.path(), &configured, ClientOptOut::default()).expect("initial configure");
    let before = fs::read_to_string(home.path().join(".codex/config.toml")).expect("read before");

    configure_all(
        home.path(),
        &configured,
        ClientOptOut {
            codex_telemetry_only: true,
            ..ClientOptOut::default()
        },
    )
    .expect("telemetry-only configure");
    let after = fs::read_to_string(home.path().join(".codex/config.toml")).expect("read after");
    let before: toml_edit::DocumentMut = before.parse().expect("before TOML");
    let after: toml_edit::DocumentMut = after.parse().expect("after TOML");
    assert_eq!(
        before["model_provider"].as_str(),
        after["model_provider"].as_str()
    );
    assert_eq!(
        before["model_providers"][CODEX_PROVIDER_ID].to_string(),
        after["model_providers"][CODEX_PROVIDER_ID].to_string(),
        "provider block changed"
    );
}

#[test]
fn codex_provider_block_is_absent_without_a_gateway_url() {
    // Inference wiring is opt-in. A telemetry-only `configure` must not
    // invent a provider block -- doing so would point Codex at a gateway
    // the caller never named, and (given /v1/responses 404s today) at a
    // broken one.
    let home = tempdir();
    fs::create_dir_all(home.path().join(".codex"))
        .expect("create .codex in the test's own temp dir");

    configure_codex(home.path(), &settings()).expect("write codex config");

    let text = fs::read_to_string(home.path().join(".codex/config.toml"))
        .expect("read back the config just written");
    assert!(
        !text.contains("model_providers"),
        "telemetry-only configure must not write a provider block, got:\n{text}"
    );
}
