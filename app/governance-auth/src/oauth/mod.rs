//! Orchestrates the login/token/refresh/status/logout commands over the flow
//! submodules. `token` is the credential-helper entrypoint wired into Claude
//! Code's `apiKeyHelper` and Codex's `[model_providers.<id>.auth] command`:
//! it must fail closed (non-zero exit, nothing on stdout) whenever it can't
//! produce a genuinely valid token, and must never launch an interactive
//! browser from an unattended re-invoke -- only `login` does that.

mod authcode;
mod callback_page;
mod callback_port;
mod device;
mod discovery;
mod exchange;
mod pkce;
mod refresh;
mod token_endpoint;

use anyhow::{Context, Result, bail};
pub use discovery::OidcMetadata;
pub use refresh::run as refresh;

use crate::{
    cache::{self, CachedSession, FileLock},
    cli,
    config::OauthConfig,
    config_file, config_persist, otel,
    redacted::Redacted,
    schedule,
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

    // Remember what worked, so `token`/`status`/`logout` stop demanding
    // `--issuer`/`--client-id` that this command has just proved. Same
    // non-fatal posture as `apply_telemetry` below and for the same reason:
    // the credential is already cached and usable, so a read-only config
    // directory must not turn a successful login into a failed command.
    remember_settings(config);

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

/// Points Claude Code and Codex at this org's collector and/or this org's AI
/// gateway. Called by `login` automatically rather than left as an opt-in
/// step: exporting telemetry is the condition for using the gateway, so
/// authenticating and being configured to report are deliberately the same
/// action.
///
/// Telemetry wiring (`otel_endpoint`) and inference/gateway wiring
/// (`gateway_url`) are independent knobs -- a caller can supply either, both,
/// or (an error) neither. They used to be wrongly coupled: an early return on
/// a missing `otel_endpoint` skipped the inference wiring too, even though
/// `apiKeyHelper`/`ANTHROPIC_BASE_URL`/Codex's provider block have nothing to
/// do with telemetry. That's why the "nothing configured at all" check below
/// is the only thing that can end this function before doing real work.
fn apply_telemetry(config: &OauthConfig, session: &CachedSession) -> Result<()> {
    let telemetry_requested = config.otel_endpoint.is_some();
    let inference_requested = config.gateway_url.is_some();

    // The developer explicitly asked to be configured and named neither an
    // OTEL collector nor a gateway -- there is nothing for this function to
    // do, and doing nothing silently (the old behaviour) left `login` users
    // stuck with an unconfigured `apiKeyHelper` and no indication why. Naming
    // both flags here, not just one, is what tells the caller how to fix it.
    if !telemetry_requested && !inference_requested {
        bail!(
            "nothing to configure: supply --otel-endpoint / GOVERNANCE_AUTH_OTEL_ENDPOINT to \
             write telemetry config, and/or --gateway-url / GOVERNANCE_AUTH_GATEWAY_URL to \
             write inference (apiKeyHelper / model-provider) config"
        );
    }

    if !telemetry_requested {
        eprintln!(
            "No OTEL endpoint configured (--otel-endpoint / GOVERNANCE_AUTH_OTEL_ENDPOINT); \
             skipping telemetry setup."
        );
    }
    if !inference_requested {
        eprintln!(
            "No gateway URL configured (--gateway-url / GOVERNANCE_AUTH_GATEWAY_URL); skipping \
             inference (apiKeyHelper / model-provider) setup."
        );
    }

    let home = std::env::var("HOME")
        .ok()
        .filter(|home| !home.is_empty())
        .map(std::path::PathBuf::from)
        .context("locating the home directory for telemetry config ($HOME unset)")?;

    let mut resource_attributes = otel::identity_attributes(session.access_token.expose());
    resource_attributes.insert("service.namespace".to_owned(), "ai-cli".to_owned());

    let settings = otel::OtelSettings {
        issuer: config.issuer.clone(),
        client_id: config.client_id.clone(),
        endpoint: config.otel_endpoint.clone(),
        // One resolution for both halves of the Copilot path: what VS Code is
        // told to write and what `copilot push` drains. See
        // `crate::copilot::resolve_spool_path`.
        copilot_spool: crate::copilot::resolve_spool_path(config)?,
        token: config.otel_token.clone().map(Redacted::new),
        resource_attributes,
        // Point Claude Code at this very binary for fresh headers. `None`
        // when telemetry wasn't requested: writing a helper for a collector
        // that isn't configured would give Claude Code a working refresh
        // loop pointed at nothing. The spelling is `crate::cli::invoke`'s, so
        // renaming the command updates this file without anyone remembering
        // to.
        headers_helper: telemetry_requested
            .then(|| cli::otel_headers_command(&config.issuer, &config.client_id)),
        headers_helper_debounce_ms: config.otel_headers_debounce_ms,
        // Built unconditionally -- harmless when inference wiring isn't
        // requested, since nothing reads it in that case (`OtelSettings`'s
        // writers gate on `gateway_url`, not on this string's presence).
        token_command: cli::token_command(&config.issuer, &config.client_id),
        gateway_url: config.gateway_url.clone(),
    };

    // ⚠️ Held, not `?`d, and surfaced at the end instead. `configure_all`
    // REFUSES a JSONC `settings.json` rather than destroying its comments --
    // and that developer is precisely the one who then pastes the exporter
    // keys by hand and ends up spooling to a disk nothing drains, which is the
    // failure this whole path exists to remove. The schedule below does not
    // depend on that write having succeeded, so it must not be skipped by it.
    let written = otel::configure_all(&home, &settings);
    let mut needs_static_token = false;
    for outcome in written.iter().flatten() {
        match outcome {
            otel::Outcome::Written(path) => {
                eprintln!("Configured: {}", path.display());
                // Codex is now the ONLY client without a dynamic-headers hook:
                // Claude Code refreshes via `otelHeadersHelper`, and VS Code
                // Copilot no longer exports for itself at all -- it writes a
                // file that `copilot push` ships with a bearer it refreshes.
                // Only when telemetry was actually requested, because a
                // gateway-only run writes `config.toml` for the provider block
                // alone, which needs no OTLP token.
                needs_static_token |= telemetry_requested
                    && path.file_name().is_some_and(|name| name == "config.toml");
            }
            otel::Outcome::Skipped(dir) => {
                eprintln!("Skipped: {} not present.", dir.display());
            }
        }
    }

    // The upload half of the Copilot path. Copilot appends to the spool and
    // stops; nothing in VS Code ships it, so without this the file exporter
    // configured above would fill a disk and export nothing. Non-fatal for the
    // same reason `login` tolerates a failed `apply_telemetry`: every config
    // file above is already written, and a machine with no user systemd
    // session must not turn a successful `configure` into a failure.
    if let Err(error) = schedule::apply(&home, config) {
        eprintln!("warning: could not update the Copilot drain schedule: {error:#}");
    }

    // Codex reads a static `Authorization` string once at start and has no
    // hook to refresh it, so this warning is about exactly one client now.
    // Naming the others would be crying wolf, and a warning people stop
    // reading is worse than none.
    if config.otel_token.is_none() && needs_static_token {
        eprintln!(
            "warning: no --otel-token supplied, so no OTLP credential was written for Codex, \
             which cannot refresh its own. Its telemetry will be rejected by an authenticating \
             collector. Claude Code (otelHeadersHelper) and VS Code Copilot (the spool drain) \
             are unaffected."
        );
    }

    // Now, and not before the schedule: see the note on `written`.
    written?;
    Ok(())
}

pub async fn token(http: &reqwest::Client, config: &OauthConfig) -> Result<()> {
    let session = current_session(http, config).await?;
    let access_token = emit_token(http, config, session).await?;

    // The ONLY thing this command ever writes to stdout. Everything else --
    // prompts, errors, status -- goes to stderr, matching the contract both
    // `apiKeyHelper` and Codex's `auth.command` expect.
    println!("{}", access_token.expose());
    Ok(())
}

/// The access token `token`/`otel headers` actually emit: the cached
/// upstream token unchanged, UNLESS token exchange (RFC 8693, opt-in, OFF by
/// default) is configured -- in which case it's the EXCHANGED token, never
/// the raw upstream one.
///
/// Fails closed by construction, not by a separate check: `exchange::run`
/// returns a `Result`, this function propagates it with `?` before either
/// caller's one `println!` runs, and there is no branch anywhere in between
/// that falls back to `session.access_token`. A misconfigured or rejected
/// exchange therefore always means non-zero exit, nothing on stdout -- the
/// same contract `current_session`'s refusal-to-refresh already has.
///
/// Takes `session` BY VALUE, not `&CachedSession`: both call sites drop
/// `session` immediately after this returns, and the exchange-OFF branch
/// (the default, and the common case -- this runs on Claude Code's
/// `otelHeadersHelper` timer and every `apiKeyHelper`/`auth.command` call)
/// used to `session.access_token.clone()` a `Redacted<String>` for no
/// reason a borrow wouldn't have avoided. Moving `session.access_token` out
/// instead means that branch is a move, not a clone.
pub(crate) async fn emit_token(
    http: &reqwest::Client,
    config: &OauthConfig,
    session: CachedSession,
) -> Result<Redacted<String>> {
    match &config.token_exchange {
        Some(exchange_config) => exchange::run(
            http,
            exchange_config,
            session.access_token.expose(),
        )
        .await
        .context("token exchange failed; refusing to fall back to the un-exchanged upstream token"),
        None => Ok(session.access_token),
    }
}

/// Claude Code's `otelHeadersHelper` entrypoint: the same refresh-or-fail
/// path as [`token`], emitted as the JSON object that hook requires
/// (`{"Authorization": "Bearer …"}`).
///
/// This is what makes telemetry auth self-renewing rather than depending on
/// a human rotating a long-lived key: Claude Code re-invokes this on an
/// interval, so a short-lived OAuth2 access token is not just workable here,
/// it's the right credential. Fails closed exactly like `token` -- a
/// rejected refresh writes nothing to stdout and exits non-zero, which the
/// hook surfaces in `/status` rather than silently exporting unauthenticated.
pub async fn otel_headers(http: &reqwest::Client, config: &OauthConfig) -> Result<()> {
    let session = current_session(http, config).await?;
    let access_token = emit_token(http, config, session).await?;
    let headers = serde_json::json!({
        "Authorization": format!("Bearer {}", access_token.expose()),
    });
    // stdout carries the JSON object and nothing else, same contract as
    // `token` -- anything extra makes the hook's parse fail.
    println!("{headers}");
    Ok(())
}

/// Loads the cached session, refreshing it if it's within the expiry skew.
/// Shared by `token` and `otel headers` so the two can't drift on when a
/// refresh happens or on what "fails closed" means. `refresh` deliberately
/// does NOT go through here: it must renew a session that is still fresh,
/// which is the one thing this function will not do.
pub(crate) async fn current_session(
    http: &reqwest::Client,
    config: &OauthConfig,
) -> Result<CachedSession> {
    let _lock = FileLock::acquire(&config.issuer, &config.client_id)?;

    let Some(session) = cache::load(&config.issuer, &config.client_id)? else {
        bail!("no cached session for this issuer/client; run `governance-auth login` first");
    };

    if session.is_fresh()? {
        return Ok(session);
    }
    let refreshed = refresh_or_fail(http, config, &session).await?;
    cache::store(&refreshed)?;
    Ok(refreshed)
}

pub(super) async fn refresh_or_fail(
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
    apply_telemetry(config, &session)?;
    // Persist here as well as on `login`. Writing only on `login` meant an
    // existing install never got a config file: upgrading keeps a valid cached
    // session, so `login` is never run again, so every later command kept
    // demanding --issuer/--client-id. Observed on a real upgrade.
    remember_settings(config);
    Ok(())
}

/// Writes the resolved settings to the per-user config file, reporting rather
/// than failing.
///
/// Non-fatal by design, in both callers: the credential is already cached and
/// usable by this point, so a read-only config directory must not turn a
/// successful command into a failed one.
fn remember_settings(config: &OauthConfig) {
    match config_file::per_user_config_path() {
        Ok(path) => match config_persist::remember(config, &path) {
            Ok(()) => eprintln!("Settings saved to {}.", path.display()),
            Err(error) => eprintln!("warning: could not save settings: {error:#}"),
        },
        Err(error) => eprintln!("warning: could not locate the config file: {error:#}"),
    }
}

/// Revokes the refresh token at the authorization server, THEN clears local
/// state.
///
/// Deleting the local file alone -- what this used to do -- leaves the
/// refresh token valid at Keycloak until its offline-session lifetime
/// expires, while telling the developer "session cleared". A logout that
/// reports success and leaves a usable credential live on the server is
/// worse than one that fails loudly, because nobody goes looking.
///
/// Order matters: revoke first, clear second. Clearing first would destroy
/// the only copy of the token needed to revoke it, so a revocation failure
/// would be unrecoverable rather than retryable.
///
/// A revocation failure is reported loudly but does NOT stop the local
/// clear. The developer asked to be logged out of this machine; refusing to
/// do that because the network is down would strand them logged in, which
/// is the worse of the two failures.
pub async fn logout(http: &reqwest::Client, config: &OauthConfig) -> Result<()> {
    let _lock = FileLock::acquire(&config.issuer, &config.client_id)?;

    match cache::load(&config.issuer, &config.client_id)? {
        Some(session) => match session.refresh_token.as_ref() {
            Some(refresh_token) => {
                if let Err(error) = revoke(http, config, refresh_token.expose()).await {
                    eprintln!(
                        "warning: could not revoke the refresh token at the authorization \
                         server, so it may remain valid there until it expires: {error:#}"
                    );
                } else {
                    eprintln!("refresh token revoked at {}", config.issuer);
                }
            }
            None => eprintln!("cached session has no refresh token; nothing to revoke"),
        },
        None => eprintln!("no cached session; nothing to revoke"),
    }

    cache::clear(&config.issuer, &config.client_id)?;
    eprintln!("session cleared");
    Ok(())
}

/// RFC 7009 revocation. Silently a no-op when the authorization server
/// doesn't advertise `revocation_endpoint` -- that's a property of the
/// server, not an error the developer can act on.
async fn revoke(http: &reqwest::Client, config: &OauthConfig, refresh_token: &str) -> Result<()> {
    let metadata = discovery::discover(http, &config.issuer).await?;
    let Some(endpoint) = metadata.revocation_endpoint.as_deref() else {
        eprintln!(
            "note: {} does not advertise a revocation endpoint; clearing locally only.",
            config.issuer
        );
        return Ok(());
    };

    let response = http
        .post(endpoint)
        .form(&[
            ("token", refresh_token),
            ("token_type_hint", "refresh_token"),
            ("client_id", config.client_id.as_str()),
        ])
        .send()
        .await
        .context("calling the revocation endpoint")?;

    let status = response.status();
    if !status.is_success() {
        // Deliberately does not include the body: an authorization server's
        // error response can echo the submitted token back.
        bail!("revocation endpoint returned {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> CachedSession {
        CachedSession {
            issuer: "https://issuer.example.com".to_owned(),
            client_id: "client".to_owned(),
            access_token: Redacted::new("access-token".to_owned()),
            refresh_token: None,
            expires_at: 0,
        }
    }

    fn config() -> OauthConfig {
        OauthConfig {
            issuer: "https://issuer.example.com".to_owned(),
            client_id: "client".to_owned(),
            scopes: "openid".to_owned(),
            audience: None,
            otel_endpoint: None,
            otel_token: None,
            gateway_url: None,
            copilot_spool_path: None,
            otel_headers_debounce_ms: 240_000,
            open_browser: false,
            token_exchange: None,
        }
    }

    /// THE regression test for the bug this module fixes. Neither flag set
    /// used to be a silent no-op (`Ok(())`, nothing written, nothing
    /// returned) -- exactly what a developer who explicitly ran `configure`
    /// and got total silence hit in production. It must now be a loud,
    /// non-zero-exit error that names both flags, so `configure` propagates
    /// it (this function's caller) while `login` still only warns (see the
    /// comment on `login`'s call site).
    #[test]
    fn configure_fails_loudly_when_neither_otel_endpoint_nor_gateway_url_is_set() {
        let error = apply_telemetry(&config(), &session())
            .expect_err("neither flag set must be a hard error, not a silent no-op");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("--otel-endpoint"),
            "must name the OTEL flag so the developer knows what to supply, got: {rendered}"
        );
        assert!(
            rendered.contains("--gateway-url"),
            "must name the gateway flag so the developer knows what to supply, got: {rendered}"
        );
    }
}
