//! The `open_browser` counterpart to `clap_default_trap.rs`: proofs that go
//! through REAL clap parsing rather than a hand-built `OauthConfigArgs`.

use super::*;

/// A per-user file must win over a machine-wide file for
/// `open_browser` too.
#[test]
fn per_user_file_wins_over_machine_file_for_open_browser() {
    let dir = tempdir();
    let per_user = write_config(&dir, "per-user.toml", "open_browser = true\n");
    let machine = write_config(&dir, "machine.toml", "open_browser = false\n");

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert!(resolved.open_browser);
}

/// A flag/env value must win over both file layers for
/// `open_browser` too.
#[test]
fn flag_or_env_value_wins_over_both_files_for_open_browser() {
    let dir = tempdir();
    let per_user = write_config(&dir, "per-user.toml", "open_browser = true\n");
    let machine = write_config(&dir, "machine.toml", "open_browser = true\n");

    let args = OauthConfigArgs {
        open_browser: Some(false),
        ..base_args()
    };
    let resolved = args
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert!(
        !resolved.open_browser,
        "an explicit false from a flag/env must win over both files' true"
    );
}

/// `--open-browser` used bare (no `=true`) must resolve to `true`
/// through REAL clap parsing -- the tri-state `Option<bool>`
/// (`num_args = 0..=1` + `default_missing_value`) equivalent of
/// `clap_default_value_would_defeat_the_config_file_layer`: this is
/// the one test in this module that would catch a regression to a
/// plain `ArgAction::SetTrue` bool flag, which would compile fine
/// but bake in `false` the instant the flag is absent -- the same
/// trap `default_value` sets for a string/int field.
#[test]
fn open_browser_flag_used_bare_resolves_to_true_through_real_clap_parsing() {
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
        "--open-browser",
    ])
    .expect("parse with a bare --open-browser flag");

    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = absent_path(&dir);
    let resolved = cli
        .oauth
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert!(
        resolved.open_browser,
        "a bare --open-browser must resolve to true"
    );
}

/// Same real-clap-parsing proof for the OTHER direction: with
/// `--open-browser` never mentioned at all, a real parse must leave
/// it `None` internally so the machine-wide file still gets a
/// chance -- catches a regression to a plain bool field with an
/// implicit `false` default just as surely as the bare-flag test
/// above catches the opposite mistake.
#[test]
fn open_browser_absent_from_a_real_clap_parse_still_falls_through_to_the_machine_file() {
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
    .expect("parse with no --open-browser flag");

    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = write_config(&dir, "machine.toml", "open_browser = true\n");
    let resolved = cli
        .oauth
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert!(
        resolved.open_browser,
        "with --open-browser absent from the real CLI parse, the machine-wide file's \
         `open_browser = true` must still take effect"
    );
}
