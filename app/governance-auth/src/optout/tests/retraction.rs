//! The half of `--no-vscode` that deletes a developer's configuration when it
//! is wrong.
//!
//! Its own file for the same reason [`crate::vscode::tests::retraction`] is:
//! this is the only test here that needs the managed-key manifest, and the
//! only one whose failure mode is destructive rather than merely incomplete.

use std::{collections::BTreeMap, fs, path::Path};

use super::{NO_VSCODE, installed, settings};
use crate::{managed, optout::ClientOptOut, otel::configure_all, vscode};

fn vscode_settings(home: &Path) -> serde_json::Value {
    let text = fs::read_to_string(vscode::user_dir(home, "Code").join("settings.json"))
        .expect("VS Code settings.json");
    serde_json::from_str(&text).expect("valid JSON")
}

/// **THE test.** `configure_all` records the keys it owns, and
/// `managed::retract_stale` removes anything recorded last time that this run
/// did not write again. So a client skipped only at the WRITE drops out of that
/// record, and the next run concludes we stopped managing its keys and deletes
/// them from the developer's file -- the opposite of what the flag says, and
/// not recoverable from.
///
/// Falsified by making `managed::plan`'s `Owned::CarriedForward` arm `continue`
/// without carrying the previous entry forward -- the naive skip. Run that way
/// it prints two `Removed (no longer managed):` lines and fails on
/// `exporterType`, with `outfile` gone from `settings.json` too. Two, not four,
/// because only string values are ever recorded, so the two boolean keys were
/// never retractable in the first place. Checked, not assumed -- and the other
/// four tests in this module still passed while it failed, which is why this
/// one is here.
#[test]
fn a_declined_client_keeps_the_keys_an_earlier_run_wrote() {
    let home = installed();
    let settings = settings(home.path());

    configure_all(home.path(), &settings, ClientOptOut::default()).expect("first run");
    let before = vscode_settings(home.path());
    assert_eq!(
        before["github.copilot.chat.otel.exporterType"], "file",
        "fixture: the first run has to have configured VS Code for real"
    );
    let owned_before = recorded_for_vscode(home.path());
    assert!(!owned_before.is_empty(), "fixture: we must own keys there");

    configure_all(home.path(), &settings, NO_VSCODE).expect("second run, VS Code opted out");

    let after = vscode_settings(home.path());
    for (key, _) in vscode::settings(&settings.copilot_spool) {
        assert_eq!(
            after.get(key),
            before.get(key),
            "--no-vscode retracted {key} from the developer's settings.json. A skipped client \
             has to be excluded from the retraction, not only from the write.\ngot: {after}"
        );
    }

    // The other half of "untouched": the manifest still records exactly what
    // the earlier run recorded, so a run WITHOUT the flag can still take those
    // keys back. An emptied entry would look like "we own nothing here" and
    // retract nothing ever again.
    assert_eq!(
        recorded_for_vscode(home.path()),
        owned_before,
        "the opted-out target's manifest entry must be carried forward verbatim"
    );
}

/// What the manifest currently records as ours in stable VS Code's
/// `settings.json`. Empty when the target is not recorded at all.
fn recorded_for_vscode(home: &Path) -> BTreeMap<String, String> {
    let target = vscode::user_dir(home, "Code").join("settings.json");
    managed::load(&managed::manifest_path(home))
        .targets
        .remove(&target.display().to_string())
        .unwrap_or_default()
}
