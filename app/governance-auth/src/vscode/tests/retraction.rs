//! The cutover: a machine already configured by a build that wrote the direct
//! HTTP exporter must end up with the file exporter and **not** both.
//!
//! Why this needs its own test rather than following from the writer's own:
//! `configure` only ever *adds* keys, so `otlpEndpoint` would survive for ever
//! on every machine that ever ran the old build. Removing it is
//! `crate::managed`'s job, and it only removes a key whose current value still
//! hashes to what we recorded writing -- so the retraction and the writer have
//! to agree about the key set, and nothing else checks that they do.
//!
//! Copilot honours one `exporterType`, so a leftover `otlpEndpoint` would not
//! actually export anything. It would read as "the direct path is configured",
//! which is the exact misreading this whole change exists to remove.
//!
//! Falsification: add `otlpEndpoint` back to `super::super::settings()` and
//! this fails on the `is_none()` assertion -- checked, not assumed.

use std::{collections::BTreeMap, fs};

use super::{settings, settings_gateway_only};
use crate::{
    managed::{self, Manifest, digest, testutil::tempdir},
    optout::ClientOptOut,
    otel::configure_all,
    vscode::user_dir,
};

/// Exactly what the pre-cutover build left behind: the two keys it wrote,
/// planted in `settings.json` *and* recorded in the manifest as ours. Both
/// halves are required -- a key we never claimed to have written is the
/// developer's, and `managed` correctly refuses to touch it.
fn plant_old_build(home: &std::path::Path) {
    let user = user_dir(home, "Code");
    fs::create_dir_all(&user).expect("create VS Code User dir");
    let path = user.join("settings.json");
    let old = [
        ("github.copilot.chat.otel.exporterType", "otlp-http"),
        (
            "github.copilot.chat.otel.otlpEndpoint",
            "https://otel.example.com",
        ),
    ];

    let object: serde_json::Map<String, serde_json::Value> = old
        .iter()
        .map(|(key, value)| {
            (
                (*key).to_owned(),
                serde_json::Value::String((*value).to_owned()),
            )
        })
        .chain(std::iter::once((
            "editor.fontSize".to_owned(),
            serde_json::Value::from(14),
        )))
        .collect();
    fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::Value::Object(object)).expect("serialize"),
    )
    .expect("seed old settings");

    let keys: BTreeMap<String, String> = old
        .iter()
        .map(|(key, value)| ((*key).to_owned(), digest(value)))
        .collect();
    let mut targets = BTreeMap::new();
    targets.insert(path.display().to_string(), keys);
    managed::save(
        &managed::manifest_path(home),
        &Manifest {
            version: 1,
            targets,
        },
    )
    .expect("seed the manifest");
}

#[test]
fn the_direct_exporter_is_retracted_not_left_beside_the_file_one() {
    let home = tempdir();
    plant_old_build(home.path());

    configure_all(home.path(), &settings(), ClientOptOut::default()).expect("configure");

    let value: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(user_dir(home.path(), "Code").join("settings.json")).expect("read"),
    )
    .expect("valid JSON out");

    assert!(
        value.get("github.copilot.chat.otel.otlpEndpoint").is_none(),
        "the stale direct-export key must be retracted, got: {value}"
    );
    assert_eq!(
        value["github.copilot.chat.otel.exporterType"], "file",
        "and the surviving exporter key must be the new value, not the old one"
    );
    assert_eq!(
        value["github.copilot.chat.otel.outfile"],
        "/state/governance-auth/copilot-otel.jsonl"
    );
    assert_eq!(
        value["editor.fontSize"], 14,
        "a key we never claimed is never touched"
    );
}

const LIGHTBRIDGE_KEYS: [&str; 2] = ["lightbridge.gatewayUrl", "lightbridge.governanceAuthPath"];

fn read_settings(home: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(
        &fs::read_to_string(user_dir(home, "Code").join("settings.json")).expect("read"),
    )
    .expect("valid JSON out")
}

fn manifest(home: &std::path::Path) -> Manifest {
    managed::load(&managed::manifest_path(home))
}

/// The `lightbridge.*` keys a gateway-only run writes must be recorded in the
/// manifest, or a later run that stops writing them could never retract them.
#[test]
fn gateway_only_wiring_is_recorded_in_the_manifest() {
    let home = tempdir();
    let user = user_dir(home.path(), "Code");
    fs::create_dir_all(&user).expect("create VS Code User dir");
    fs::write(user.join("settings.json"), r#"{}"#).expect("seed settings");

    configure_all(
        home.path(),
        &settings_gateway_only(),
        ClientOptOut::default(),
    )
    .expect("configure");

    let value = read_settings(home.path());
    for key in LIGHTBRIDGE_KEYS {
        assert!(value.get(key).is_some(), "{key} must be written");
    }

    let target = user.join("settings.json").display().to_string();
    let manifest = manifest(home.path());
    let recorded = manifest.targets.get(&target).expect("target recorded");
    for key in LIGHTBRIDGE_KEYS {
        assert!(
            recorded.contains_key(key),
            "{key} must be recorded in managed.json"
        );
    }
}

/// Falsification: if `entries` stopped gating the `lightbridge.*` keys on
/// `gateway_url`, or `managed::plan` stopped recording them, this fails --
/// the stale keys would survive a run that no longer owns them.
#[test]
fn dropping_the_gateway_retracts_the_lightbridge_keys() {
    let home = tempdir();
    let user = user_dir(home.path(), "Code");
    fs::create_dir_all(&user).expect("create VS Code User dir");
    fs::write(user.join("settings.json"), r#"{}"#).expect("seed settings");

    configure_all(
        home.path(),
        &settings_gateway_only(),
        ClientOptOut::default(),
    )
    .expect("configure with gateway");
    assert!(
        read_settings(home.path())
            .get("lightbridge.gatewayUrl")
            .is_some()
    );

    // Now run with neither gateway nor collector: the telemetry writer has
    // nothing to do, and the gateway keys must be retracted, not left behind.
    configure_all(home.path(), &settings(), ClientOptOut::default()).expect("configure without");

    let value = read_settings(home.path());
    for key in LIGHTBRIDGE_KEYS {
        assert!(
            value.get(key).is_none(),
            "stale {key} must be retracted once the gateway is gone"
        );
    }
}

/// `--no-vscode` promises to leave VS Code entirely alone -- that includes not
/// retracting keys a prior run wrote (write side is covered in `optout`'s own
/// tests; this pins the retraction half).
#[test]
fn no_vscode_leaves_lightbridge_keys_alone() {
    let home = tempdir();
    let user = user_dir(home.path(), "Code");
    fs::create_dir_all(&user).expect("create VS Code User dir");
    fs::write(user.join("settings.json"), r#"{}"#).expect("seed settings");

    configure_all(
        home.path(),
        &settings_gateway_only(),
        ClientOptOut::default(),
    )
    .expect("configure with gateway");
    assert!(
        read_settings(home.path())
            .get("lightbridge.gatewayUrl")
            .is_some()
    );

    // A gateway-less run WITH `--no-vscode`: the flag must stop the writer
    // AND the retraction, so the keys survive.
    let optout = ClientOptOut {
        vscode: true,
        ..ClientOptOut::default()
    };
    configure_all(home.path(), &settings(), optout).expect("configure with --no-vscode");

    let value = read_settings(home.path());
    for key in LIGHTBRIDGE_KEYS {
        assert!(
            value.get(key).is_some(),
            "{key} must survive a --no-vscode run even after the gateway is gone"
        );
    }
}
