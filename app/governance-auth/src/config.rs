//! CLI-configurable OAuth2 client identity. No issuer/client id is baked in:
//! the Keycloak realm and client this binary talks to are registered
//! per-deployment (see the ai-helm coordination note in
//! `docs/adr/0010-governance-auth-keycloak-oauth2-credential-helper.md`).

use clap::Args;
use url::Url;

use crate::security;

#[derive(Debug, Clone, Args)]
pub struct OauthConfig {
    /// Base URL of the issuing OIDC realm, e.g.
    /// `https://auth.ai.camer.digital/realms/platform`. OIDC discovery is
    /// used to find the authorization/token/device endpoints underneath it.
    /// Must be `https://`, unless it's a loopback address
    /// (`127.0.0.1`/`::1`/`localhost`) -- see [`crate::security`]. Validated
    /// here, at parse time, rather than left to fail at first network use:
    /// this is a credential helper, and an operator's typo shouldn't be
    /// discovered only when a token request silently goes out in plaintext.
    #[arg(long, env = "GOVERNANCE_AUTH_ISSUER", value_parser = parse_issuer)]
    pub issuer: String,

    /// Public OAuth2 client id registered for this binary. Must be a public
    /// client (no client secret ships in a binary distributed to laptops).
    #[arg(long, env = "GOVERNANCE_AUTH_CLIENT_ID")]
    pub client_id: String,

    /// Space-separated OAuth2 scopes to request.
    #[arg(
        long,
        env = "GOVERNANCE_AUTH_SCOPES",
        default_value = "openid profile offline_access"
    )]
    pub scopes: String,

    /// Optional `resource`/`audience` parameter, if the authorization server
    /// needs one to scope the issued token to the gateway.
    #[arg(long, env = "GOVERNANCE_AUTH_AUDIENCE")]
    pub audience: Option<String>,
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
