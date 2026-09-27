//! `issuer`/`client_id` go through the same layering and validation as every
//! other option, plus the malformed-file guard.

use super::*;

/// `issuer`/`client_id` go through the exact same layering, not a
/// separate code path -- pinned here so a future refactor that
/// special-cases them can't quietly drop the file layers for the
/// two fields that matter most (nothing else works without them).
#[test]
fn issuer_and_client_id_also_fall_through_to_config_files() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = write_config(
        &dir,
        "machine.toml",
        "issuer = \"https://from-machine-config.example\"\nclient_id = \"machine-client\"\n",
    );

    let args = OauthConfigArgs {
        issuer: None,
        client_id: None,
        ..base_args()
    };
    let resolved = args
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.issuer, "https://from-machine-config.example");
    assert_eq!(resolved.client_id, "machine-client");
}

/// A config-file-sourced `issuer` gets the same HTTPS-or-loopback
/// validation a CLI/env one already gets at clap-parse time --
/// config files bypass clap entirely, so without this a plaintext
/// typo in `/etc/governance-auth/config.toml` would reach the
/// network unchecked.
#[test]
fn a_config_file_issuer_is_still_validated_for_transport_security() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = write_config(
        &dir,
        "machine.toml",
        "issuer = \"http://not-loopback.example\"\n",
    );

    let args = OauthConfigArgs {
        issuer: None,
        ..base_args()
    };
    let error = args
        .resolve_with_paths(&per_user, &machine)
        .expect_err("a plaintext non-loopback issuer from a config file must be rejected");
    assert!(format!("{error:#}").contains("HTTPS"));
}

/// Still required when absent from every layer -- config files
/// don't relax the "issuer/client-id must be present" rule, they
/// just add two more places it can come from.
#[test]
fn missing_issuer_everywhere_is_still_an_error() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = absent_path(&dir);

    let args = OauthConfigArgs {
        issuer: None,
        ..base_args()
    };
    let error = args
        .resolve_with_paths(&per_user, &machine)
        .expect_err("no issuer anywhere must be an error");
    assert!(format!("{error:#}").contains("--issuer"));
}

/// A malformed config file must fail loudly rather than being
/// treated as absent -- silently falling through to the next,
/// weaker layer would hide a real typo in a file an operator
/// believes is in effect.
#[test]
fn a_malformed_per_user_file_is_a_loud_error_not_a_silent_fallthrough() {
    let dir = tempdir();
    let per_user = write_config(&dir, "per-user.toml", "not = [valid toml");
    let machine = absent_path(&dir);

    let error = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect_err("malformed TOML must be an error, not silently skipped");
    assert!(format!("{error:#}").contains(&per_user.display().to_string()));
}
