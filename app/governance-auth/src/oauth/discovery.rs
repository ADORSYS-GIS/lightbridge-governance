//! OIDC discovery (`/.well-known/openid-configuration`). Endpoints are never
//! hand-derived from the issuer URL -- discovery is what lets this binary
//! work against any Keycloak realm without a code change if paths move.

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct OidcMetadata {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub device_authorization_endpoint: Option<String>,
}

pub async fn discover(http: &reqwest::Client, issuer: &str) -> Result<OidcMetadata> {
    let issuer = issuer.trim_end_matches('/');
    let url = format!("{issuer}/.well-known/openid-configuration");

    let response = http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("fetching OIDC discovery document from {url}"))?
        .error_for_status()
        .with_context(|| format!("OIDC discovery document at {url} returned an error status"))?;

    response
        .json::<OidcMetadata>()
        .await
        .with_context(|| format!("parsing OIDC discovery document from {url}"))
}
