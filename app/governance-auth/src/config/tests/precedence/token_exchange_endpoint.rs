//! Token exchange: which endpoint wins, file-layer fallthrough, and the
//! real-clap-parse regression guard.

use super::*;

/// An explicit `--exchange-token-endpoint` takes precedence over
/// `--exchange-issuer` when both are set -- documented behaviour
/// (that flag's `--help` and `docs/governance-auth/configuration.md`),
/// pinned here so a refactor can't silently flip which one wins.
#[test]
fn token_exchange_explicit_token_endpoint_wins_over_issuer() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = absent_path(&dir);

    let args = OauthConfigArgs {
        token_exchange: Some(true),
        exchange_issuer: Some("https://exchange.example".to_owned()),
        exchange_token_endpoint: Some("https://exchange.example/oauth2/token".to_owned()),
        exchange_client_id: Some("exchange-cli".to_owned()),
        ..base_args()
    };
    let resolved = args
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    let exchange = resolved.token_exchange.expect("token exchange resolved");
    assert!(matches!(
        exchange.token_endpoint,
        ExchangeTokenEndpoint::Explicit(ref endpoint)
            if endpoint == "https://exchange.example/oauth2/token"
    ));
}

/// Every exchange field falls through the same file layering as
/// `issuer`/`client_id` -- a machine-wide file alone can fully
/// configure token exchange, with no flag/env involved at all.
#[test]
fn token_exchange_config_falls_through_to_the_machine_wide_file() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = write_config(
        &dir,
        "machine.toml",
        "token_exchange = true\n\
         exchange_token_endpoint = \"https://exchange.example/oauth2/token\"\n\
         exchange_client_id = \"machine-exchange-cli\"\n",
    );

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    let exchange = resolved.token_exchange.expect("token exchange resolved");
    assert_eq!(exchange.client_id, "machine-exchange-cli");
}

/// A per-user file's `token_exchange = false` must win over a
/// machine-wide file's `token_exchange = true` -- same precedence
/// every other field has, proved specifically for the boolean gate
/// rather than just the fields underneath it.
#[test]
fn per_user_file_wins_over_machine_file_for_token_exchange_enabled() {
    let dir = tempdir();
    let per_user = write_config(&dir, "per-user.toml", "token_exchange = false\n");
    let machine = write_config(
        &dir,
        "machine.toml",
        "token_exchange = true\n\
         exchange_token_endpoint = \"https://exchange.example/oauth2/token\"\n\
         exchange_client_id = \"machine-exchange-cli\"\n",
    );

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert!(
        resolved.token_exchange.is_none(),
        "per-user token_exchange = false must win over the machine file's true"
    );
}

/// The `token_exchange` counterpart to
/// `open_browser_absent_from_a_real_clap_parse_still_falls_through_to_the_machine_file`
/// -- and, like that test, the only one in this module that can
/// catch a `default_value`/`default_value_t` regression reintroduced
/// on the real `#[arg(...)]` for `token_exchange`, because it's the
/// only one that goes through REAL clap parsing
/// (`TestCli::try_parse_from`) rather than hand-constructing
/// `OauthConfigArgs { token_exchange: None, .. }`.
///
/// Every OTHER `token_exchange` test above builds `OauthConfigArgs`
/// by hand, which proves the layering logic in
/// `resolve_token_exchange` is correct but can never observe a
/// mistake in the `#[arg(...)]` attribute itself: clap fills a
/// `default_value` in BEFORE `OauthConfigArgs` exists as a value a
/// test could construct differently, so a hand-built
/// `token_exchange: None` stays `None` even if the real CLI would
/// never produce it. Confirmed by sabotaging with
/// `default_value = "false"` on the `token_exchange` arg: with that
/// in place, `--token-exchange` never mentioned still parses to
/// `Some(false)`, so `resolve_token_exchange` sees `enabled = false`
/// and never even reads the machine-wide file's `token_exchange =
/// true` -- this test failed with `resolved.token_exchange` being
/// `None` instead of `Some`, exactly the trap this module's doc
/// warns about.
#[test]
fn token_exchange_absent_from_a_real_clap_parse_still_falls_through_to_the_machine_file() {
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
    .expect("parse with no --token-exchange flag");

    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = write_config(
        &dir,
        "machine.toml",
        "token_exchange = true\n\
         exchange_token_endpoint = \"https://exchange.example/oauth2/token\"\n\
         exchange_client_id = \"machine-exchange-cli\"\n",
    );

    let resolved = cli
        .oauth
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert!(
        resolved.token_exchange.is_some(),
        "with --token-exchange absent from the real CLI parse, the machine-wide file's \
         `token_exchange = true` must still take effect -- if this fails with `None` \
         instead, `default_value` has been reintroduced on the `token_exchange` arg"
    );
}
