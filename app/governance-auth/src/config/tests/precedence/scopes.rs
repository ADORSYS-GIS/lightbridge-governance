//! Layer-by-layer precedence for `scopes`.

use super::*;

/// Layer 1 vs layer 2 (clap flag vs its own env var) is proved in
/// `tests/config_precedence.rs`, end-to-end through a real
/// subprocess with `GOVERNANCE_AUTH_SCOPES` set on the *child's*
/// environment via `Command::env`, not here: that precedence step
/// happens INSIDE clap, before `OauthConfigArgs` is ever
/// constructed by hand, and proving it would otherwise require
/// mutating this test binary's own process environment -- which
/// `std::env::set_var` can do, but only as `unsafe` since Rust 2024,
/// and this workspace denies `unsafe_code` outright (see root
/// `Cargo.toml`). A subprocess's environment is safe, ordinary
/// `Command::env`, so that's where this one lives.
///
/// This is also the exact case the `default_value`/`default_value_t`
/// trap made impossible to observe: with a clap default in place,
/// `scopes` was never `None`, so there was nothing for a config file
/// (or, transitively, an env-var-vs-file test) to ever win against.
/// Layer 2 vs layer 3: an env-var-sourced value (indistinguishable,
/// by the time `OauthConfigArgs` exists, from a flag-sourced one --
/// see the test above) must win over a per-user config file.
#[test]
fn env_or_flag_value_wins_over_per_user_file_for_scopes() {
    let dir = tempdir();
    let per_user = write_config(&dir, "per-user.toml", "scopes = \"per-user-scope\"\n");
    let machine = write_config(&dir, "machine.toml", "scopes = \"machine-scope\"\n");

    let args = OauthConfigArgs {
        scopes: Some("cli-or-env-scope".to_owned()),
        ..base_args()
    };
    let resolved = args
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.scopes, "cli-or-env-scope");
}

/// Layer 3 vs layer 4: with no flag/env value, the per-user file
/// must win over the machine-wide file.
#[test]
fn per_user_file_wins_over_machine_file_for_scopes() {
    let dir = tempdir();
    let per_user = write_config(&dir, "per-user.toml", "scopes = \"per-user-scope\"\n");
    let machine = write_config(&dir, "machine.toml", "scopes = \"machine-scope\"\n");

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.scopes, "per-user-scope");
}

/// Layer 4 vs layer 5: with no flag/env value and no per-user file,
/// the machine-wide file must win over the compiled default.
#[test]
fn machine_file_wins_over_compiled_default_for_scopes() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = write_config(&dir, "machine.toml", "scopes = \"machine-scope\"\n");

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.scopes, "machine-scope");
}
