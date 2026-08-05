//! Device Authorization Grant (RFC 8628): the `--device-code` fallback for
//! headless sessions (SSH, Coder cloud workspaces) with no local browser to
//! open. Mirrors this org's earlier `kc-token` CLI, which defaulted to this
//! flow for exactly that reason.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::{OidcMetadata, token_endpoint};
use crate::{cache::CachedSession, config::OauthConfig};

#[derive(Debug, Deserialize)]
struct DeviceAuthorizationResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default = "default_poll_interval")]
    interval: u64,
}

fn default_poll_interval() -> u64 {
    5
}

pub async fn run(
    http: &reqwest::Client,
    config: &OauthConfig,
    metadata: &OidcMetadata,
) -> Result<CachedSession> {
    let device_endpoint = metadata
        .device_authorization_endpoint
        .as_deref()
        .context("authorization server does not advertise a device_authorization_endpoint")?;

    let mut params = vec![
        ("client_id", config.client_id.as_str()),
        ("scope", config.scopes.as_str()),
    ];
    if let Some(audience) = &config.audience {
        params.push(("audience", audience.as_str()));
    }

    let response = http
        .post(device_endpoint)
        .form(&params)
        .send()
        .await
        .with_context(|| format!("calling device authorization endpoint {device_endpoint}"))?
        .error_for_status()
        .context("device authorization endpoint returned an error status")?;

    let device: DeviceAuthorizationResponse = response
        .json()
        .await
        .context("parsing device authorization response")?;

    match &device.verification_uri_complete {
        Some(uri) => eprintln!("To sign in, visit:\n{uri}"),
        None => eprintln!(
            "To sign in, visit {} and enter code: {}",
            device.verification_uri, device.user_code
        ),
    }

    poll(http, metadata, config, &device).await
}

async fn poll(
    http: &reqwest::Client,
    metadata: &OidcMetadata,
    config: &OauthConfig,
    device: &DeviceAuthorizationResponse,
) -> Result<CachedSession> {
    let mut interval = Duration::from_secs(device.interval.max(1));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(device.expires_in);

    loop {
        tokio::time::sleep(interval).await;
        if tokio::time::Instant::now() >= deadline {
            bail!("device code expired before login completed");
        }

        let poll_params = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device.device_code.as_str()),
            ("client_id", config.client_id.as_str()),
        ];

        match token_endpoint::request(http, &metadata.token_endpoint, &poll_params).await {
            Ok(response) => return token_endpoint::into_session(config, response),
            Err(error) => match error.code.as_deref() {
                Some("authorization_pending") => continue,
                Some("slow_down") => {
                    interval += Duration::from_secs(5);
                    continue;
                }
                _ => bail!(error),
            },
        }
    }
}
