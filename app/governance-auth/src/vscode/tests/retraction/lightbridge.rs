//! The `lightbridge.*` retraction story (issue #233): the two keys a
//! gateway-only run writes are recorded in the manifest so a later run that
//! stops owning them can retract them, and `--no-vscode` must leave them
//! alone on both the write and the retract halves.
//!
//! Split out of [`super`] purely for the LoC ceiling -- this is the same
//! managed-key story as the cutover test in the parent module, but for the
//! inference keys rather than the exporter cutover.

use std::fs;

use super::super::{settings, settings_gateway_only};
use crate::{
    managed::{self, Manifest, testutil::tempdir},
    optout::ClientOptOut,
    otel::configure_all,
    vscode::user_dir,
};

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

/// The "previously-owned, now-unreadable" transition. Run 1 records the
/// `lightbridge.*` keys in the manifest; the developer then annotates
/// settings.json into JSONC (which this binary refuses to rewrite) and drops
/// the gateway. Run 2 can't physically retract the keys, so it must NOT drop
/// the target from the ledger -- otherwise the stale keys are unreachable
/// forever. The ownership record is carried forward for a later run to retry.
#[test]
fn an_unreadable_settings_json_keeps_the_lightbridge_ownership() {
    let home = tempdir();
    let user = user_dir(home.path(), "Code");
    fs::create_dir_all(&user).expect("create VS Code User dir");

    // Run 1: gateway set, plain JSON -> keys written and recorded.
    fs::write(user.join("settings.json"), r#"{}"#).expect("seed plain JSON");
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
    let target = user.join("settings.json").display().to_string();
    assert!(
        manifest(home.path()).targets.contains_key(&target),
        "run 1 must have recorded the vscode target"
    );

    // Between runs the developer annotates the file (JSONC) and drops the
    // gateway: run 2 no longer owns the keys, but cannot read the file to
    // remove them.
    let annotated = "{\n  // my note\n  \"editor.fontSize\": 14\n}\n";
    fs::write(user.join("settings.json"), annotated).expect("annotate to JSONC");
    configure_all(home.path(), &settings(), ClientOptOut::default()).expect("configure without");

    assert_eq!(
        fs::read_to_string(user.join("settings.json")).expect("read back"),
        annotated,
        "the annotated file must still be left untouched"
    );
    assert!(
        manifest(home.path()).targets.contains_key(&target),
        "the ledger must keep the vscode target (carried forward) for a later retry"
    );
}
