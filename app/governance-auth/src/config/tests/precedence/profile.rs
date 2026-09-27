//! Layer-by-layer precedence for `profile` (ADR-0016).

use super::*;

/// `profile`'s own five-layer round trip, collapsed into one test
/// per pair rather than the four separate `scopes` tests above --
/// same layering call, different field.
#[test]
fn profile_per_user_file_wins_over_machine_file() {
    let dir = tempdir();
    let per_user = write_config(&dir, "per-user.toml", "profile = \"manual\"\n");
    let machine = write_config(&dir, "machine.toml", "profile = \"daemon\"\n");

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.profile, crate::profile::Profile::Manual);
}

#[test]
fn profile_machine_file_wins_over_compiled_default() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = write_config(&dir, "machine.toml", "profile = \"manual\"\n");

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.profile, crate::profile::Profile::Manual);
}

/// `Profile::default()` is `Manual` until #268/#272 land (see that
/// module's doc) -- pinned here the same way
/// `open_browser_compiled_default_is_false` pins its own field below,
/// so a change to `Profile::default()` fails a config test, not just
/// `profile.rs`'s own unit test.
#[test]
fn profile_compiled_default_is_manual_when_nothing_else_is_configured() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = absent_path(&dir);

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.profile, crate::profile::Profile::Manual);
}

/// A config-file value bypasses clap's `value_parser` entirely, so
/// this is the layer that actually rejects a bad one -- falsified by
/// first checking the message names the culprit, mirroring
/// `profile::tests::an_unrecognised_value_is_rejected_by_name`.
#[test]
fn profile_an_invalid_config_file_value_is_rejected_by_name() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = write_config(&dir, "machine.toml", "profile = \"sometimes\"\n");

    let error = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect_err("an unrecognised profile must not silently resolve");
    assert!(format!("{error:#}").contains("sometimes"));
}
