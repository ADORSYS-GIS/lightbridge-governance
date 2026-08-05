//! Shared token-endpoint plumbing: the raw POST, structured error decoding,
//! and turning a token response into a [`CachedSession`]. Authorization-code,
//! device-code and refresh flows all funnel through here so retry/error
//! handling (`authorization_pending`, `slow_down`, ...) is decoded once.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Deserialize;

use super::OidcMetadata;
use crate::{cache::CachedSession, config::OauthConfig};

#[derive(Debug, Deserialize)]
pub struct RawTokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
struct TokenErrorBody {
    error: Option<String>,
    error_description: Option<String>,
}

/// A structured token-endpoint failure. Kept distinct from `anyhow::Error`
/// so the device-code poll loop can match on `code` (`authorization_pending`,
/// `slow_down`) instead of substring-matching a formatted message.
#[derive(Debug)]
pub struct TokenEndpointError {
    pub status: reqwest::StatusCode,
    pub code: Option<String>,
    pub description: Option<String>,
}

impl std::fmt::Display for TokenEndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "token endpoint returned {} ({}): {}",
            self.status,
            self.code.as_deref().unwrap_or("unknown_error"),
            self.description.as_deref().unwrap_or("")
        )
    }
}

impl std::error::Error for TokenEndpointError {}

fn transport_error(description: String) -> TokenEndpointError {
    TokenEndpointError {
        status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        code: None,
        description: Some(description),
    }
}

pub async fn request(
    http: &reqwest::Client,
    token_endpoint: &str,
    params: &[(&str, &str)],
) -> Result<RawTokenResponse, TokenEndpointError> {
    let response = http
        .post(token_endpoint)
        .form(params)
        .send()
        .await
        .map_err(|error| transport_error(format!("calling {token_endpoint}: {error}")))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| transport_error(format!("reading response body: {error}")))?;

    if !status.is_success() {
        let parsed: TokenErrorBody = serde_json::from_str(&body).unwrap_or_default();
        return Err(TokenEndpointError {
            status,
            code: parsed.error,
            description: parsed.error_description,
        });
    }

    serde_json::from_str(&body).map_err(|error| TokenEndpointError {
        status,
        code: None,
        description: Some(format!("parsing token response: {error}")),
    })
}

pub fn into_session(config: &OauthConfig, response: RawTokenResponse) -> Result<CachedSession> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("reading system clock")?
        .as_secs();
    Ok(CachedSession {
        issuer: config.issuer.clone(),
        client_id: config.client_id.clone(),
        access_token: response.access_token,
        refresh_token: response.refresh_token,
        expires_at: now.saturating_add(response.expires_in.unwrap_or(60)),
    })
}

pub async fn refresh(
    http: &reqwest::Client,
    metadata: &OidcMetadata,
    config: &OauthConfig,
    refresh_token: &str,
) -> Result<CachedSession> {
    let mut params = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", config.client_id.as_str()),
    ];
    if let Some(audience) = &config.audience {
        params.push(("audience", audience.as_str()));
    }

    let response = request(http, &metadata.token_endpoint, &params).await?;
    let mut session = into_session(config, response)?;
    // Keycloak rotates refresh tokens on every use; if this particular
    // response omitted one, keep using the one we already had rather than
    // discarding a still-valid rotation chain.
    if session.refresh_token.is_none() {
        session.refresh_token = Some(refresh_token.to_owned());
    }
    Ok(session)
}
