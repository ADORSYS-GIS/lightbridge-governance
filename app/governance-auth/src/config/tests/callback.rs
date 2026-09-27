//! `--callback-port`/`--callback-bind` (issue #364), proved the same way
//! `precedence` proves every other option: hand-built `OauthConfigArgs`
//! driving `resolve_with_paths` directly against temp-file config layers,
//! plus one real-clap-parse test for the `default_value` trap this crate's
//! module doc warns about. Shares `precedence`'s helpers rather than
//! duplicating them.

use std::net::IpAddr;

use super::{
    super::*,
    precedence::{absent_path, base_args, tempdir, write_config},
};
use crate::oauth::CALLBACK_PORTS;

/// Unset, both compiled defaults must match every build before these flags
/// existed: `callback_port` tries the whole block, `callback_bind` is
/// loopback.
#[test]
fn compiled_defaults_are_try_the_block_and_loopback() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = absent_path(&dir);

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.callback_port, None);
    assert_eq!(resolved.callback_bind, IpAddr::from([127, 0, 0, 1]));
}

#[test]
fn callback_port_machine_file_wins_over_compiled_default() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = write_config(
        &dir,
        "machine.toml",
        &format!("callback_port = {}\n", CALLBACK_PORTS[2]),
    );

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.callback_port, Some(CALLBACK_PORTS[2]));
}

#[test]
fn callback_port_per_user_file_wins_over_machine_file() {
    let dir = tempdir();
    let per_user = write_config(
        &dir,
        "per-user.toml",
        &format!("callback_port = {}\n", CALLBACK_PORTS[0]),
    );
    let machine = write_config(
        &dir,
        "machine.toml",
        &format!("callback_port = {}\n", CALLBACK_PORTS[1]),
    );

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.callback_port, Some(CALLBACK_PORTS[0]));
}

#[test]
fn callback_port_flag_or_env_value_wins_over_both_files() {
    let dir = tempdir();
    let per_user = write_config(
        &dir,
        "per-user.toml",
        &format!("callback_port = {}\n", CALLBACK_PORTS[0]),
    );
    let machine = write_config(
        &dir,
        "machine.toml",
        &format!("callback_port = {}\n", CALLBACK_PORTS[1]),
    );

    let args = OauthConfigArgs {
        callback_port: Some(CALLBACK_PORTS[4]),
        ..base_args()
    };
    let resolved = args
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.callback_port, Some(CALLBACK_PORTS[4]));
}

/// A config-file `callback_port` bypasses clap's `value_parser` entirely, so
/// this is the layer that actually rejects one outside `CALLBACK_PORTS` --
/// mirrors `a_config_file_issuer_is_still_validated_for_transport_security`
/// in `precedence`.
#[test]
fn callback_port_from_a_config_file_outside_the_block_is_rejected_and_names_it() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = write_config(&dir, "machine.toml", "callback_port = 9999\n");

    let error = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect_err("a port outside CALLBACK_PORTS from a config file must be rejected");
    let rendered = format!("{error:#}");
    assert!(rendered.contains("9999"), "got: {rendered}");
    for port in CALLBACK_PORTS {
        assert!(rendered.contains(&port.to_string()), "got: {rendered}");
    }
}

/// The `default_value`/`default_value_t` regression guard this crate's
/// module doc requires for every numeric option: only a REAL clap parse
/// (`OauthConfigArgs::try_parse_from`) can observe a `default_value` on the
/// `#[arg(...)]` itself, because a hand-built `callback_port: None` above
/// stays `None` even if the real CLI would never produce it.
#[test]
fn callback_port_absent_from_a_real_clap_parse_still_falls_through_to_the_machine_file() {
    use clap::Parser as _;

    #[derive(Debug, clap::Parser)]
    struct TestCli {
        #[command(flatten)]
        oauth: OauthConfigArgs,
    }

    let cli = TestCli::try_parse_from([
        "governance-auth",
        "--issuer",
        "https://issuer.example",
        "--client-id",
        "cli",
    ])
    .expect("parse with no --callback-port flag");

    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = write_config(
        &dir,
        "machine.toml",
        &format!("callback_port = {}\n", CALLBACK_PORTS[3]),
    );

    let resolved = cli
        .oauth
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(
        resolved.callback_port,
        Some(CALLBACK_PORTS[3]),
        "with --callback-port absent from the real CLI parse, the machine-wide file's value \
         must still take effect -- if this fails with `None` instead, `default_value` has been \
         reintroduced on the `callback_port` arg"
    );
}

#[test]
fn callback_bind_per_user_file_wins_over_machine_file() {
    let dir = tempdir();
    let per_user = write_config(&dir, "per-user.toml", "callback_bind = \"0.0.0.0\"\n");
    let machine = write_config(&dir, "machine.toml", "callback_bind = \"127.0.0.1\"\n");

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.callback_bind, IpAddr::from([0, 0, 0, 0]));
}

#[test]
fn callback_bind_flag_or_env_value_wins_over_both_files() {
    let dir = tempdir();
    let per_user = write_config(&dir, "per-user.toml", "callback_bind = \"0.0.0.0\"\n");
    let machine = write_config(&dir, "machine.toml", "callback_bind = \"0.0.0.0\"\n");

    let args = OauthConfigArgs {
        callback_bind: Some("127.0.0.1".to_owned()),
        ..base_args()
    };
    let resolved = args
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert_eq!(resolved.callback_bind, IpAddr::from([127, 0, 0, 1]));
}

/// A config-file `callback_bind` bypasses clap too, so an unparseable value
/// must still be rejected here rather than silently accepted as a hostname.
#[test]
fn callback_bind_from_a_config_file_that_is_not_an_ip_is_rejected() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = write_config(&dir, "machine.toml", "callback_bind = \"not-an-ip\"\n");

    let error = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect_err("a non-IP callback_bind from a config file must be rejected");
    assert!(format!("{error:#}").contains("callback-bind"));
}
