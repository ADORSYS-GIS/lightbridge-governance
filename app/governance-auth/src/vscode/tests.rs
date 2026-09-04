//! What `configure` writes into a VS Code flavour's `settings.json`, and what
//! it refuses to write.
//!
//! The cutover from the direct HTTP exporter to the file one has its own file,
//! [`retraction`]: it is the only test here that needs the managed-key
//! manifest, and it is the one that proves an already-configured machine ends
//! up with the new exporter and not both.

mod daemon;
mod retraction;

use std::{collections::BTreeMap, fs, path::PathBuf};

use super::*;
use crate::managed::testutil::tempdir;

/// Telemetry configured, gateway not: VS Code's OTEL surface is the only
/// thing this writer touches, so that is the interesting axis.
pub(super) fn settings() -> OtelSettings {
    OtelSettings {
        issuer: "https://auth.example".to_owned(),
        client_id: "cli".to_owned(),
        endpoint: Some("https://otel.example.com".to_owned()),
        copilot_spool: PathBuf::from("/state/governance-auth/copilot-otel.jsonl"),
        copilot_drain_available: true,
        copilot_otlp_direct: false,
        token: None,
        resource_attributes: BTreeMap::new(),
        headers_helper: None,
        headers_helper_debounce_ms: 240_000,
        token_command: "/abs/path/governance-auth token".to_owned(),
        gateway_url: None,
    }
}

fn settings_gateway_only() -> OtelSettings {
    OtelSettings {
        endpoint: None,
        copilot_drain_available: false,
        copilot_otlp_direct: false,
        gateway_url: Some("https://api.example".to_owned()),
        ..settings()
    }
}

#[test]
fn vscode_settings_are_merged_into_an_existing_user_config() {
    let home = tempdir();
    let user = user_dir(home.path(), "Code");
    fs::create_dir_all(&user).expect("create VS Code User dir");
    fs::write(
        user.join("settings.json"),
        r#"{"editor.fontSize":14,"github.copilot.enable":{"*":true}}"#,
    )
    .expect("seed existing VS Code settings");

    let outcomes = configure(home.path(), &settings()).expect("configure vscode");
    assert!(matches!(outcomes.as_slice(), [Outcome::Written(_)]));

    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(user.join("settings.json")).expect("read"))
            .expect("valid JSON out");
    assert_eq!(value["editor.fontSize"], 14, "unrelated settings survive");
    assert_eq!(value["github.copilot.enable"]["*"], true);
    assert_eq!(value["github.copilot.chat.otel.enabled"], true);
    // `file`, not `otlp-http`. The direct exporter carries no header this
    // binary is willing to write into a Settings-Sync'd file, so it 401s --
    // see `super::configure`.
    assert_eq!(value["github.copilot.chat.otel.exporterType"], "file");
    assert_eq!(
        value["github.copilot.chat.otel.outfile"],
        "/state/governance-auth/copilot-otel.jsonl"
    );
    assert!(
        value.get("github.copilot.chat.otel.otlpEndpoint").is_none(),
        "the 401-ing direct exporter must not be configured alongside the file one"
    );
    assert_eq!(
        value["github.copilot.chat.otel.captureContent"], false,
        "content capture must stay off unless deliberately enabled"
    );
}

#[test]
fn a_jsonc_vscode_config_is_refused_rather_than_stripped_of_its_comments() {
    // VS Code's settings.json legitimately allows comments. Parsing them out
    // and writing plain JSON back would delete a developer's annotations
    // permanently, so this must decline and tell them what to add -- the file
    // has to come back untouched.
    let home = tempdir();
    let user = user_dir(home.path(), "Code");
    fs::create_dir_all(&user).expect("create VS Code User dir");
    let original = "{\n  // my carefully explained setting\n  \"editor.fontSize\": 14\n}\n";
    fs::write(user.join("settings.json"), original).expect("seed JSONC settings");

    let error = configure(home.path(), &settings())
        .expect_err("a JSONC config must be refused, not silently rewritten");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("github.copilot.chat.otel.outfile"),
        "the error must tell the developer exactly what to add; got: {rendered}"
    );

    assert_eq!(
        fs::read_to_string(user.join("settings.json")).expect("read back"),
        original,
        "the file must be left byte-for-byte untouched"
    );
}

#[test]
fn vscode_insiders_and_vscodium_are_configured_too() {
    // A developer on Insiders or VSCodium would otherwise get nothing,
    // silently, because those keep entirely separate settings trees.
    let home = tempdir();
    for flavour in ["Code - Insiders", "VSCodium"] {
        fs::create_dir_all(user_dir(home.path(), flavour)).expect("create user dir");
    }

    let outcomes = configure(home.path(), &settings()).expect("configure vscode");
    assert_eq!(outcomes.len(), 2, "both flavours present must be written");
    for flavour in ["Code - Insiders", "VSCodium"] {
        let path = user_dir(home.path(), flavour).join("settings.json");
        assert!(path.exists(), "{flavour} settings.json should exist");
    }
}

#[test]
fn the_file_exporter_is_not_enabled_without_a_collector_to_drain_to() {
    // Not merely "nothing to configure": turning the file exporter on with no
    // endpoint would have Copilot spool telemetry to disk for ever with
    // nothing draining it -- the disk cost of the feature and none of its
    // value. A gateway-only configure must leave settings.json alone.
    let home = tempdir();
    let user = user_dir(home.path(), "Code");
    fs::create_dir_all(&user).expect("create VS Code User dir");
    fs::write(user.join("settings.json"), r#"{"editor.fontSize":14}"#).expect("seed settings");

    let outcomes = configure(home.path(), &settings_gateway_only()).expect("configure");
    assert!(outcomes.is_empty(), "nothing to write, so nothing reported");

    let text = fs::read_to_string(user.join("settings.json")).expect("read back");
    assert_eq!(
        text, r#"{"editor.fontSize":14}"#,
        "file must be left untouched"
    );
}

// #272 AC3's daemon-profile Copilot path has its own file, `daemon.rs`, for
// the same LoC-ceiling reason `retraction.rs` does.
