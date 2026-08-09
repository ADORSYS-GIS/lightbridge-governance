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
    otel,
    redacted::Redacted,
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

    // Deliberately not `?`: the session is already cached and valid by this
    // point, and failing `login` because a dotfile couldn't be written would
    // leave the developer with no working credential and no obvious cause --
    // strictly worse than an un-instrumented client. Reported loudly instead.
    // `configure` (the subcommand) propagates the same error, because there
    // the developer asked for exactly this and nothing else.
    if let Err(error) = apply_telemetry(config, &session) {
        eprintln!("warning: could not configure telemetry: {error:#}");
    }
    Ok(())
}

/// Points Claude Code and Codex at this org's collector. Called by `login`
/// automatically rather than left as an opt-in step: exporting telemetry is
/// the condition for using the gateway, so authenticating and being
/// configured to report are deliberately the same action.
fn apply_telemetry(config: &OauthConfig, session: &CachedSession) -> Result<()> {
    let Some(endpoint) = config.otel_endpoint.clone() else {
        eprintln!(
            "No OTEL endpoint configured (--otel-endpoint / GOVERNANCE_AUTH_OTEL_ENDPOINT); \
             skipping telemetry setup."
        );
        return Ok(());
    };

    let home = std::env::var("HOME")
        .ok()
        .filter(|home| !home.is_empty())
        .map(std::path::PathBuf::from)
        .context("locating the home directory for telemetry config ($HOME unset)")?;

    let mut resource_attributes = otel::identity_attributes(session.access_token.expose());
    resource_attributes.insert("service.namespace".to_owned(), "ai-cli".to_owned());

    let settings = otel::OtelSettings {
        endpoint,
        token: config.otel_token.clone().map(Redacted::new),
        resource_attributes,
    };

    let outcomes = otel::configure_all(&home, &settings)?;
    let mut wrote_vscode = false;
    for outcome in &outcomes {
        match outcome {
            otel::Outcome::Written(path) => {
                eprintln!("Telemetry configured: {}", path.display());
                // VS Code's settings live under `<flavour>/User/`, which is
                // how a written VS Code config is told apart from the two
                // CLI ones without threading a tool tag through `Outcome`.
                wrote_vscode |= path.parent().is_some_and(|dir| dir.ends_with("User"));
            }
            otel::Outcome::Skipped(dir) => {
                eprintln!("Skipped telemetry setup: {} not present.", dir.display());
            }
        }
    }

    // VS Code exposes the endpoint as a setting but authentication ONLY as an
    // environment variable, so this is the one target whose config this binary
    // genuinely cannot finish. Saying so is the difference between "Copilot
    // telemetry is rejected and nobody knows why" and a one-line fix.
    if wrote_vscode && let Some(env) = otel::vscode_manual_env(&settings) {
        eprintln!(
            "\nACTION REQUIRED for VS Code Copilot: it has no setting for OTLP auth headers.\n\
             Export this in the environment you launch VS Code from, or its telemetry will be\n\
             rejected by the collector:\n\n  export {env}\n"
        );
    }

    if config.otel_token.is_none() {
        eprintln!(
            "warning: no --otel-token supplied, so no OTLP credential was written. \
             Telemetry will be rejected by an authenticating collector."
        );
    }
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

/// Re-applies the telemetry configuration for an already-cached session.
/// Unlike `login`'s call, a failure here IS an error: the developer asked for
/// exactly this and nothing else, so silently doing nothing would be a lie.
pub fn configure(config: &OauthConfig) -> Result<()> {
    let session = cache::load(&config.issuer, &config.client_id)?
        .context("no cached session for this issuer/client; run `governance-auth login` first")?;
    apply_telemetry(config, &session)
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
