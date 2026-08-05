//! Orchestrates the login/token/status/logout commands over the flow
//! submodules. `token` is the credential-helper entrypoint wired into Claude
//! Code's `apiKeyHelper` and Codex's `[model_providers.<id>.auth] command`:
//! it must fail closed (non-zero exit, nothing on stdout) whenever it can't
//! produce a genuinely valid token, and must never launch an interactive
//! browser from an unattended re-invoke -- only `login` does that.

mod authcode;
mod device;
mod discovery;
mod pkce;
mod token_endpoint;

use anyhow::{Context, Result, bail};
pub use discovery::OidcMetadata;

use crate::{
    cache::{self, CachedSession, FileLock},
    config::OauthConfig,
};

pub async fn login(http: &reqwest::Client, config: &OauthConfig, device_code: bool) -> Result<()> {
    let _lock = FileLock::acquire(&config.issuer, &config.client_id)?;
    let metadata = discovery::discover(http, &config.issuer).await?;

    let session = if device_code {
        device::run(http, config, &metadata).await?
    } else {
        authcode::run(http, config, &metadata).await?
    };

    let expires_in = session.seconds_until_expiry()?;
    cache::store(&session)?;
    eprintln!("Logged in; session cached, expires in {expires_in}s.");
    Ok(())
}

pub async fn token(http: &reqwest::Client, config: &OauthConfig) -> Result<()> {
    let _lock = FileLock::acquire(&config.issuer, &config.client_id)?;

    let Some(session) = cache::load(&config.issuer, &config.client_id)? else {
        bail!("no cached session for this issuer/client; run `governance-auth login` first");
    };

    let session = if session.is_fresh()? {
        session
    } else {
        let refreshed = refresh_or_fail(http, config, &session).await?;
        cache::store(&refreshed)?;
        refreshed
    };

    // The ONLY thing this command ever writes to stdout. Everything else --
    // prompts, errors, status -- goes to stderr, matching the contract both
    // `apiKeyHelper` and Codex's `auth.command` expect.
    println!("{}", session.access_token.expose());
    Ok(())
}

async fn refresh_or_fail(
    http: &reqwest::Client,
    config: &OauthConfig,
    session: &CachedSession,
) -> Result<CachedSession> {
    let refresh_token = session
        .refresh_token
        .as_ref()
        .context("cached session has no refresh token; run `governance-auth login` again")?
        .expose()
        .as_str();

    let metadata = discovery::discover(http, &config.issuer).await?;
    token_endpoint::refresh(http, &metadata, config, refresh_token)
        .await
        .context("refreshing the access token; run `governance-auth login` again if this persists")
}

pub fn status(config: &OauthConfig) -> Result<()> {
    match cache::load(&config.issuer, &config.client_id)? {
        Some(session) => {
            let fresh = session.is_fresh()?;
            let expires_in = session.seconds_until_expiry()?;
            eprintln!(
                "session cached, {}, expires in {expires_in}s",
                if fresh { "fresh" } else { "needs refresh" },
            );
        }
        None => eprintln!("no cached session"),
    }
    Ok(())
}

pub fn logout(config: &OauthConfig) -> Result<()> {
    cache::clear(&config.issuer, &config.client_id)?;
    eprintln!("session cleared");
    Ok(())
}
