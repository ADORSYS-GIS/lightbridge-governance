//! Layer-by-layer precedence for `open_browser` (issue #141).

use super::*;

/// `open_browser` (issue #141): with nothing configured anywhere,
/// the compiled default is `false` -- pinned so the browser default
/// stays off without needing a clap `default_value` (see this
/// module's doc and `clap_default_value_would_defeat_the_config_file_layer`
/// for why that specific mechanism must not be used here either).
#[test]
fn open_browser_compiled_default_is_false() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = absent_path(&dir);

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert!(!resolved.open_browser);
}

/// A machine-wide file turning `open_browser` on must be honoured --
/// the field-specific version of the "machine file beats compiled
/// default" proof every other option already has.
#[test]
fn machine_file_wins_over_compiled_default_for_open_browser() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = write_config(&dir, "machine.toml", "open_browser = true\n");

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert!(resolved.open_browser);
}

/// `last_no_vscode` (and its three siblings) are memory for
/// `update::reapply`, not a live setting -- no flag or env var
/// resolves them, only the file layers, which this pins the same
/// way `open_browser`'s own compiled-default test does.
#[test]
fn last_opt_out_fields_compiled_default_to_false() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = absent_path(&dir);

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert!(!resolved.last_no_claude);
    assert!(!resolved.last_no_codex);
    assert!(!resolved.last_no_vscode);
    assert!(!resolved.last_codex_telemetry_only);
}

/// A per-user file recording `no_vscode = true` (what
/// `config_persist::remember` writes after a `--no-vscode` run) must
/// resolve back to `true` -- the read half of the round trip
/// `config_persist::tests::the_opt_out_choice_round_trips_per_field`
/// covers the write half of.
#[test]
fn per_user_file_resolves_last_no_vscode() {
    let dir = tempdir();
    let per_user = write_config(&dir, "per-user.toml", "no_vscode = true\n");
    let machine = absent_path(&dir);

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert!(resolved.last_no_vscode);
    assert!(
        !resolved.last_no_claude,
        "an unrelated field must not also flip"
    );
}
