//! On-disk fixtures for telemetry survey tests.

use std::collections::BTreeMap;

use crate::managed::{Manifest, digest, manifest_path, save};

/// Writes a real `settings.json` carrying `helper`, and a manifest that claims
/// we wrote that key -- the pair `stale_wiring` reads.
pub(super) fn seed_claude_settings(home: &std::path::Path, helper: &str) {
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

pub(super) fn seed_codex_auth(home: &std::path::Path, command: &str, args: Option<&[&str]>) {
    let config = home.join(".codex/config.toml");
    std::fs::create_dir_all(config.parent().expect("parent")).expect("mkdir");
    let mut document = toml_edit::DocumentMut::new();
    document["model_providers"]["governance"]["auth"]["command"] = toml_edit::value(command);
    if let Some(args) = args {
        let args: toml_edit::Array = args.iter().copied().collect();
        document["model_providers"]["governance"]["auth"]["args"] = toml_edit::value(args);
    }
    std::fs::write(&config, document.to_string()).expect("write config.toml");

    let key = "model_providers.governance.auth.command";
    let mut keys = BTreeMap::new();
    keys.insert(key.to_owned(), digest(command));
    let mut targets = BTreeMap::new();
    targets.insert(config.display().to_string(), keys);
    save(
        &manifest_path(home),
        &Manifest {
            version: 1,
            targets,
        },
    )
    .expect("save manifest");
}
