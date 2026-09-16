//! The retraction edge case that needs its own file for the LoC ceiling:
//! what happens when we own keys but can no longer read the file holding them.

use std::{collections::BTreeMap, fs};

use super::super::{testutil::*, *};

/// If a target we own keys of can no longer be READ (the developer annotated
/// settings.json into JSONC, say), we can't verify or remove them -- so the
/// target must be re-claimed into `now`, keeping the ownership record so a
/// later run retries. Dropping it would make the stale keys unreachable
/// forever.
#[test]
fn an_unreadable_target_is_reclaimed_not_dropped() {
    let dir = tempdir();
    let target = dir.path().join("settings.json");
    fs::write(
        &target,
        "{\n  // my note\n  \"lightbridge.gatewayUrl\": \"https://gw.example\"\n}\n",
    )
    .expect("seed JSONC");

    let manifest = previous(&target, &[("lightbridge.gatewayUrl", "https://gw.example")]);
    let mut now = BTreeMap::new();
    let removed = retract_stale(&manifest, &mut now);

    assert!(removed.is_empty(), "nothing could actually be removed");
    assert!(
        now.contains_key(&target.display().to_string()),
        "the target must stay in the ledger for a later retry: {now:?}"
    );
    assert!(
        fs::read_to_string(&target)
            .expect("read")
            .contains("lightbridge"),
        "the unreadable file must be left untouched"
    );
}
