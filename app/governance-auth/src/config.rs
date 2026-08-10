//! CLI-configurable OAuth2 client identity. No issuer/client id is baked in:
//! the Keycloak realm and client this binary talks to are registered
//! per-deployment (see the ai-helm coordination note in
//! `docs/adr/0010-governance-auth-keycloak-oauth2-credential-helper.md`).

use clap::Args;
use url::Url;

use crate::security;

/// What `clap` actually parses. Fields are `Option`, not `String`/required,
/// because clap rejects a `global = true` arg that's also `required`
/// (`Command governance-auth: Global arguments cannot be required`) --
/// [`Self::resolve`] is where "must actually be present" gets enforced, with
/// a message naming the flag, not a generic clap usage dump.
///
/// `global = true` on all four fields: without it, clap only accepts them
/// *before* the subcommand name (`governance-auth --issuer ... token`, not
/// `governance-auth token --issuer ...`), because this is flattened onto the
/// top-level `Cli` rather than duplicated per subcommand. That ordering
/// requirement is a footgun specifically for this binary's main use case: a
/// single command-line string embedded in `apiKeyHelper`/`auth.command`,
/// which both vendors' own docs and this repo's runbook show with the
/// subcommand written first (`"governance-auth token"`) -- composing that
/// pattern with explicit `--issuer`/`--client-id` (rather than relying on
/// `GOVERNANCE_AUTH_ISSUER`/`GOVERNANCE_AUTH_CLIENT_ID` env vars, which a
/// helper subprocess isn't guaranteed to inherit) used to fail with `error:
/// unexpected argument '--issuer' found` and no hint that reordering the
/// string would fix it. Verified against a real `apiKeyHelper` invocation,
/// not just a unit test.
#[derive(Debug, Clone, Args)]
pub struct OauthConfigArgs {
    /// Base URL of the issuing OIDC realm, e.g.
    /// `https://auth.ai.camer.digital/realms/platform`. OIDC discovery is
    /// used to find the authorization/token/device endpoints underneath it.
    /// Must be `https://`, unless it's a loopback address
    /// (`127.0.0.1`/`::1`/`localhost`) -- see [`crate::security`]. Validated
    /// here, at parse time, rather than left to fail at first network use:
    /// this is a credential helper, and an operator's typo shouldn't be
    /// discovered only when a token request silently goes out in plaintext.
    #[arg(long, env = "GOVERNANCE_AUTH_ISSUER", value_parser = parse_issuer, global = true)]
    issuer: Option<String>,

    /// Public OAuth2 client id registered for this binary. Must be a public
    /// client (no client secret ships in a binary distributed to laptops).
    #[arg(long, env = "GOVERNANCE_AUTH_CLIENT_ID", global = true)]
    client_id: Option<String>,

    /// Space-separated OAuth2 scopes to request.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_SCOPES",
        default_value = "openid profile offline_access",
        global = true
    )]
    scopes: String,

    /// Optional `resource`/`audience` parameter, if the authorization server
    /// needs one to scope the issued token to the gateway.
    #[arg(long, env = "GOVERNANCE_AUTH_AUDIENCE", global = true)]
    audience: Option<String>,

    /// OTLP collector base URL written into Claude Code's and Codex's config
    /// on `login`. Signal suffixes (`/v1/metrics`, ...) are appended by those
    /// tools' own SDKs -- pass the base, not a per-signal path. Same
    /// HTTPS-or-loopback rule as `--issuer`: telemetry carries prompts and
    /// tool detail, so it must not go out in plaintext by typo.
    #[arg(long, env = "GOVERNANCE_AUTH_OTEL_ENDPOINT", value_parser = parse_issuer, global = true)]
    otel_endpoint: Option<String>,

    /// Long-lived OTLP ingest credential. Written verbatim into both tools'
    /// config as an `Authorization: Bearer` header.
    ///
    /// Deliberately NOT the Keycloak access token: neither tool re-reads its
    /// config mid-session and neither has a credential-helper hook for OTLP
    /// headers, so a 300s token would export for five minutes and then fail
    /// silently. See `crate::otel`'s module doc.
    #[arg(long, env = "GOVERNANCE_AUTH_OTEL_TOKEN", global = true)]
    otel_token: Option<String>,

    /// How often Claude Code re-runs `otel-headers` for fresh OTLP headers.
    /// Default 240s, deliberately under Keycloak's 300s access-token
    /// lifetime -- Claude Code's own default is 29 MINUTES, which would mean
    /// exporting with an expired token for most of every half hour, and
    /// failing silently while doing it.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_OTEL_HEADERS_DEBOUNCE_MS",
        default_value_t = 240_000,
        global = true
    )]
    otel_headers_debounce_ms: u64,
}

impl OauthConfigArgs {
    /// Turns the as-parsed (possibly incomplete) args into the
    /// [`OauthConfig`] every command actually needs, or a message naming
    /// exactly which flag/env var is missing -- clap can't enforce this
    /// itself once `issuer`/`client_id` are `global` (see the struct doc).
    pub fn resolve(self) -> Result<OauthConfig, String> {
        Ok(OauthConfig {
            issuer: self
                .issuer
                .ok_or("--issuer (or GOVERNANCE_AUTH_ISSUER) is required")?,
            client_id: self
                .client_id
                .ok_or("--client-id (or GOVERNANCE_AUTH_CLIENT_ID) is required")?,
            scopes: self.scopes,
            audience: self.audience,
            otel_endpoint: self.otel_endpoint,
            otel_token: self.otel_token,
            otel_headers_debounce_ms: self.otel_headers_debounce_ms,
        })
    }
}

/// The resolved, always-present OAuth2 client identity every command
/// operates on -- what `OauthConfigArgs::resolve` produces. Kept as a
/// separate (non-`Option`) type so the 13+ call sites across `oauth/*.rs`
/// that read `config.issuer`/`config.client_id` as plain `&str` don't each
/// need to handle absence individually; that's handled once, at the CLI
/// boundary.
#[derive(Debug, Clone)]
pub struct OauthConfig {
    pub issuer: String,
    pub client_id: String,
    pub scopes: String,
    pub audience: Option<String>,
    pub otel_endpoint: Option<String>,
    pub otel_token: Option<String>,
    pub otel_headers_debounce_ms: u64,
}

/// `clap` value parser for `--issuer`/`GOVERNANCE_AUTH_ISSUER`: rejects an
/// unparseable URL or one that fails [`security::require_secure`] before
/// this binary ever tries to use it. The raw string is kept (not the
/// re-serialized `Url`) so downstream trailing-slash handling
/// (`oauth::discovery::discover`) sees exactly what the operator passed.
fn parse_issuer(raw: &str) -> Result<String, String> {
    let url = Url::parse(raw).map_err(|error| format!("invalid issuer URL: {error}"))?;
    security::require_secure(&url).map_err(|error| error.to_string())?;
    Ok(raw.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_loopback_http_issuer() {
        let error = parse_issuer("http://auth.example.com/realms/platform")
            .expect_err("plaintext non-loopback issuer must be rejected");
        assert!(
            error.contains("HTTPS"),
            "error should explain the HTTPS requirement, got: {error}"
        );
    }

    #[test]
    fn accepts_https_issuer() {
        assert!(parse_issuer("https://auth.example.com/realms/platform").is_ok());
    }

    #[test]
    fn accepts_loopback_http_issuer() {
        assert!(parse_issuer("http://127.0.0.1:4181/realms/platform").is_ok());
    }
}
