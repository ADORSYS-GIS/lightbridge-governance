//! RFC 8693 token exchange -- OPTIONAL, OFF by default (see
//! [`crate::config::ExchangeConfig`]). Trades the upstream (today,
//! Keycloak-shaped) access token this binary already holds for a downstream
//! token minted by a *separate* authorization server -- lightbridge-authz's
//! native `/oauth2/token` endpoint (ADR-0011 phase 2 there), per that repo's
//! `docs/token-exchange-integration.md`. Only `oauth::token` and
//! `oauth::otel_headers` call [`run`], and only when a caller opted in.
//!
//! ## Fail closed
//!
//! [`run`]'s only job is to return `Result<Redacted<String>>`. Both call
//! sites (`oauth::mod::emit_token`) propagate an `Err` with `?` BEFORE their
//! one `println!` runs -- there is no branch anywhere that falls back to the
//! un-exchanged upstream token. An operator who turned exchange on
//! deliberately chose not to emit that token, so a network error, a
//! malformed response, or an `invalid_grant`/`invalid_client` from the
//! exchange endpoint must all produce nothing on stdout and a non-zero exit,
//! never a silent downgrade to the credential they opted out of. This
//! mirrors the fail-closed contract `oauth::mod`'s module doc states for
//! `token` itself.
//!
//! ## What this deliberately does NOT send
//!
//! - **No `project_id`.** Required by this deployment until upstream PR
//!   #309 merged; now optional, resolving to the subject's own
//!   auto-provisioned default project. Exposing a `--exchange-project-id`
//!   knob here would just reintroduce a required field the server itself
//!   dropped.
//! - **No `audience`/`resource`.** RFC 8693 defines the parameter, but this
//!   deployment's exchange handler never reads it (verified live, and in
//!   the integration guide): the minted token's `aud`/`azp` are always
//!   exactly the requesting `client_id`, regardless of what's sent. Adding a
//!   config knob that appears to scope the token's audience but silently
//!   does nothing would be worse than omitting it -- a configuration that
//!   lies about its own effect.

use anyhow::{Context, Result};

use super::{discovery, token_endpoint};
use crate::{
    config::{ExchangeConfig, ExchangeTokenEndpoint},
    redacted::Redacted,
};

/// Exchanges `subject_token` (the caller's current upstream access token)
/// for a downstream one, per `exchange`'s resolved configuration. See this
/// module's doc for the fail-closed contract callers depend on.
pub async fn run(
    http: &reqwest::Client,
    exchange: &ExchangeConfig,
    subject_token: &str,
) -> Result<Redacted<String>> {
    let endpoint = resolve_token_endpoint(http, exchange).await?;

    let mut params = vec![
        (
            "grant_type",
            "urn:ietf:params:oauth:grant-type:token-exchange",
        ),
        ("client_id", exchange.client_id.as_str()),
        ("subject_token", subject_token),
        (
            "subject_token_type",
            "urn:ietf:params:oauth:token-type:access_token",
        ),
    ];
    if let Some(scopes) = &exchange.scopes {
        params.push(("scope", scopes.as_str()));
    }

    let response = token_endpoint::request(http, &endpoint, &params)
        .await
        .context("token exchange request failed")?;
    Ok(response.access_token)
}

/// [`ExchangeTokenEndpoint::Explicit`] is used as-is; [`ExchangeTokenEndpoint::Issuer`]
/// costs one OIDC discovery round trip (cached the same way the primary
/// issuer's is -- see `oauth::discovery`) to find the real endpoint.
async fn resolve_token_endpoint(
    http: &reqwest::Client,
    exchange: &ExchangeConfig,
) -> Result<String> {
    match &exchange.token_endpoint {
        ExchangeTokenEndpoint::Explicit(endpoint) => Ok(endpoint.clone()),
        ExchangeTokenEndpoint::Issuer(issuer) => {
            let metadata = discovery::discover(http, issuer)
                .await
                .context("discovering the token-exchange issuer")?;
            Ok(metadata.token_endpoint)
        }
    }
}

// No unit tests in this module: its two behaviours -- a successful exchange
// returning the EXCHANGED token, and a rejected exchange failing closed with
// nothing on stdout -- are both proved end-to-end in
// `tests/token_exchange.rs`, through the real CLI subprocess and
// `tests/support/mock_idp.rs` (reused as a generic mock token endpoint, not
// a new mock). That's a stronger guarantee than a unit test calling `run`
// directly: it also exercises `emit_token`'s wiring in `oauth::mod` and the
// config-resolution path in `config.rs`, so a regression in either would be
// caught even if this module's own logic were untouched. Mirrors how
// `oauth::device`/`oauth::authcode` have no unit tests of their own either
// -- `tests/device_flow.rs` and `tests/login_flow.rs`/`tests/pkce_authcode.rs`
// are the tests that exercise them.
