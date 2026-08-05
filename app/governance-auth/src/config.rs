//! CLI-configurable OAuth2 client identity. No issuer/client id is baked in:
//! the Keycloak realm and client this binary talks to are registered
//! per-deployment (see the ai-helm coordination note in
//! `docs/adr/0010-governance-auth-keycloak-oauth2-credential-helper.md`).

use clap::Args;

#[derive(Debug, Clone, Args)]
pub struct OauthConfig {
    /// Base URL of the issuing OIDC realm, e.g.
    /// `https://auth.ai.camer.digital/realms/platform`. OIDC discovery is
    /// used to find the authorization/token/device endpoints underneath it.
    #[arg(long, env = "GOVERNANCE_AUTH_ISSUER")]
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
