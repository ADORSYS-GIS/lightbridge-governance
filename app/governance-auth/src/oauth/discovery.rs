//! OIDC discovery (`/.well-known/openid-configuration`). Endpoints are never
//! hand-derived from the issuer URL -- discovery is what lets this binary
//! work against any Keycloak realm without a code change if paths move.

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct OidcMetadata {
    pub issuer: String,
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

    let metadata: OidcMetadata = response
        .json()
        .await
        .with_context(|| format!("parsing OIDC discovery document from {url}"))?;

    // OIDC Discovery (RFC 8414 §3.1.2 / OIDC Discovery 4.3) requires the
    // returned `issuer` to match what was requested -- otherwise a
    // compromised or misconfigured discovery response could redirect the
    // authorization/token/device requests to an attacker-chosen endpoint
    // without the client ever noticing.
    let discovered_issuer = metadata.issuer.trim_end_matches('/');
    if discovered_issuer != issuer {
        bail!(
            "OIDC discovery document at {url} claims issuer `{discovered_issuer}`, expected `{issuer}` -- refusing to trust it"
        );
    }

    Ok(metadata)
}
