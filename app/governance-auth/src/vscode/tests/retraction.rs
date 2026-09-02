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

use super::settings;
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
