//! RFC 8693 token-exchange configuration -- split out of `config/mod.rs`
//! (issue #364) purely to keep that file under its grandfathered LoC
//! ceiling; this is the same five-layer resolution every other option in
//! this crate uses, just for the five fields that only matter once
//! `--token-exchange` is on.

use anyhow::{Context, Result, bail};

use super::{parse_exchange_token_endpoint, parse_issuer};
use crate::config_file;

/// The token-exchange sub-block of [`super::OauthConfigArgs::resolve_with_paths`],
/// taking the five raw fields by value (rather than `&self`) because it's a
/// five-field group with its own internal validation (client id required,
/// exactly one of issuer/token-endpoint required) -- inlining it would make
/// the parent function's field-by-field shape harder to scan. A `&self`-taking
/// method would not compile at the call site: by that point several *other*
/// fields of `self` have already been individually moved out via
/// `self.field.or_else(...)` (`Option<String>` isn't `Copy`), and Rust does
/// not allow borrowing a struct as a whole once any one of its fields has
/// been partially moved, even to read a field that was never touched.
pub(super) fn resolve(
    token_exchange: Option<bool>,
    exchange_issuer: Option<String>,
    exchange_token_endpoint: Option<String>,
    exchange_client_id: Option<String>,
    exchange_scopes: Option<String>,
    per_user: Option<&config_file::ConfigFile>,
    machine: Option<&config_file::ConfigFile>,
) -> Result<Option<ExchangeConfig>> {
    let enabled = token_exchange
        .or_else(|| per_user.and_then(|file| file.token_exchange))
        .or_else(|| machine.and_then(|file| file.token_exchange))
        .unwrap_or(false);

    if !enabled {
        return Ok(None);
    }

    let exchange_issuer = exchange_issuer
        .or_else(|| per_user.and_then(|file| file.exchange_issuer.clone()))
        .or_else(|| machine.and_then(|file| file.exchange_issuer.clone()))
        .map(|value| parse_issuer(&value).map_err(|error| anyhow::anyhow!(error)))
        .transpose()?;

    let exchange_token_endpoint = exchange_token_endpoint
        .or_else(|| per_user.and_then(|file| file.exchange_token_endpoint.clone()))
        .or_else(|| machine.and_then(|file| file.exchange_token_endpoint.clone()))
        .map(|value| parse_exchange_token_endpoint(&value).map_err(|error| anyhow::anyhow!(error)))
        .transpose()?;

    let client_id = exchange_client_id
        .or_else(|| per_user.and_then(|file| file.exchange_client_id.clone()))
        .or_else(|| machine.and_then(|file| file.exchange_client_id.clone()))
        .context(
            "--exchange-client-id (or GOVERNANCE_AUTH_EXCHANGE_CLIENT_ID, or \
             `exchange_client_id` in a config file) is required when token exchange \
             (--token-exchange) is enabled",
        )?;

    let scopes = exchange_scopes
        .or_else(|| per_user.and_then(|file| file.exchange_scopes.clone()))
        .or_else(|| machine.and_then(|file| file.exchange_scopes.clone()));

    let token_endpoint = match (exchange_token_endpoint, exchange_issuer) {
        (Some(endpoint), _) => ExchangeTokenEndpoint::Explicit(endpoint),
        (None, Some(issuer)) => ExchangeTokenEndpoint::Issuer(issuer),
        (None, None) => bail!(
            "token exchange (--token-exchange) is enabled but neither \
             --exchange-token-endpoint (GOVERNANCE_AUTH_EXCHANGE_TOKEN_ENDPOINT) nor \
             --exchange-issuer (GOVERNANCE_AUTH_EXCHANGE_ISSUER) is set, in a flag, env var, or \
             config file"
        ),
    };

    Ok(Some(ExchangeConfig {
        token_endpoint,
        client_id,
        scopes,
    }))
}

/// Resolved RFC 8693 token-exchange configuration, built by [`resolve`] only
/// when `--token-exchange` (or its env var/config-file equivalent) is on.
/// See `oauth::exchange`'s module doc for the request this drives and its
/// fail-closed contract, and lightbridge-authz's
/// `docs/token-exchange-integration.md` for the wire contract itself.
#[derive(Debug, Clone)]
pub struct ExchangeConfig {
    pub token_endpoint: ExchangeTokenEndpoint,
    pub client_id: String,
    pub scopes: Option<String>,
}

/// Where the token-exchange request goes. An explicit
/// `--exchange-token-endpoint` is used as-is; `--exchange-issuer` costs one
/// OIDC discovery round trip (cached, same as the primary `--issuer` --
/// see `oauth::discovery`) to find it.
#[derive(Debug, Clone)]
pub enum ExchangeTokenEndpoint {
    Explicit(String),
    Issuer(String),
}
