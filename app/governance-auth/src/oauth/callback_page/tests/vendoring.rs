//! The seam with the other repository: the artifact is built there and
//! committed here, so these catch a stale digest, a hand-edited file, or a
//! placeholder that stopped being substituted.

use super::*;

#[test]
fn the_outcome_is_substituted_and_the_placeholder_never_survives() {
    let ok = document(true);
    let bad = document(false);
    assert!(ok.contains(r#"data-callback-status="success""#));
    assert!(bad.contains(r#"data-callback-status="error""#));
    for page in [&ok, &bad] {
        assert!(
            !page.contains(STATUS_PLACEHOLDER),
            "an unsubstituted placeholder renders the failure page, which is safe but wrong"
        );
    }
}

#[test]
fn the_placeholder_appears_exactly_once_in_the_vendored_artifact() {
    // More than once and `replace` would stamp the outcome somewhere nobody
    // reviewed; zero and the vendoring is broken in a way the substitution
    // above cannot detect, because `replace` on a missing needle is a no-op
    // that returns a perfectly valid-looking page stuck on failure.
    assert_eq!(PAGE.matches(STATUS_PLACEHOLDER).count(), 1);
}

#[test]
fn the_vendored_page_matches_its_recorded_digest() {
    // Catches a hand-edited artifact, which is the one failure the build gate
    // in the other repository cannot see.
    use sha2::{Digest, Sha256};
    let recorded = SOURCE
        .split(r#""sha256": ""#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("callback.source.json must record a sha256");
    assert_eq!(
        hex::encode(Sha256::digest(PAGE.as_bytes())),
        recorded,
        "callback.html does not match callback.source.json -- re-run scripts/vendor-callback-page.sh"
    );
}
