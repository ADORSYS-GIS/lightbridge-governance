//! Token exchange (issue #140): on/off and required-field checks.

use super::*;

/// Token exchange (issue #140) is OFF by default: with nothing
/// configured anywhere, `resolved.token_exchange` must be `None`,
/// the ONLY representation of "disabled" (see `ExchangeConfig`'s
/// doc).
#[test]
fn token_exchange_is_off_by_default() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = absent_path(&dir);

    let resolved = base_args()
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    assert!(resolved.token_exchange.is_none());
}

/// Enabling exchange without an exchange client id must be a loud
/// error naming the missing flag, not a silently-disabled exchange
/// or a panic reaching for a value that was never there.
#[test]
fn token_exchange_enabled_without_a_client_id_is_an_error() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = absent_path(&dir);

    let args = OauthConfigArgs {
        token_exchange: Some(true),
        exchange_issuer: Some("https://exchange.example".to_owned()),
        ..base_args()
    };
    let error = args
        .resolve_with_paths(&per_user, &machine)
        .expect_err("token exchange without a client id must be rejected");
    assert!(format!("{error:#}").contains("--exchange-client-id"));
}

/// Enabling exchange without EITHER an exchange issuer or an
/// explicit exchange token endpoint must also be a loud error --
/// there is nowhere to send the exchange request otherwise.
#[test]
fn token_exchange_enabled_without_an_issuer_or_token_endpoint_is_an_error() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = absent_path(&dir);

    let args = OauthConfigArgs {
        token_exchange: Some(true),
        exchange_client_id: Some("exchange-cli".to_owned()),
        ..base_args()
    };
    let error = args
        .resolve_with_paths(&per_user, &machine)
        .expect_err("token exchange without an issuer/token-endpoint must be rejected");
    assert!(format!("{error:#}").contains("--exchange-token-endpoint"));
    assert!(format!("{error:#}").contains("--exchange-issuer"));
}

/// A fully-specified exchange config (issuer form) resolves to
/// `Some(ExchangeConfig)` with every field carried through, proving
/// the happy path end-to-end through `resolve_with_paths` rather
/// than just its error branches.
#[test]
fn token_exchange_fully_specified_via_issuer_resolves() {
    let dir = tempdir();
    let per_user = absent_path(&dir);
    let machine = absent_path(&dir);

    let args = OauthConfigArgs {
        token_exchange: Some(true),
        exchange_issuer: Some("https://exchange.example".to_owned()),
        exchange_client_id: Some("exchange-cli".to_owned()),
        exchange_scopes: Some("openid profile".to_owned()),
        ..base_args()
    };
    let resolved = args
        .resolve_with_paths(&per_user, &machine)
        .expect("resolve");
    let exchange = resolved
        .token_exchange
        .expect("token exchange must be Some when fully specified");
    assert_eq!(exchange.client_id, "exchange-cli");
    assert_eq!(exchange.scopes.as_deref(), Some("openid profile"));
    assert!(matches!(
        exchange.token_endpoint,
        ExchangeTokenEndpoint::Issuer(ref issuer) if issuer == "https://exchange.example"
    ));
}
