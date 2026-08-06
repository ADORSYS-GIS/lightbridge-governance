//! OIDC discovery (`/.well-known/openid-configuration`). Endpoints are never
//! hand-derived from the issuer URL -- discovery is what lets this binary
//! work against any Keycloak realm without a code change if paths move.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use url::Url;

use crate::security;

#[derive(Debug, Clone, Deserialize)]
pub struct OidcMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub device_authorization_endpoint: Option<String>,
}

pub async fn discover(http: &reqwest::Client, issuer: &str) -> Result<OidcMetadata> {
    let issuer = issuer.trim_end_matches('/');
    let issuer_url = Url::parse(issuer).with_context(|| "parsing the configured issuer URL")?;
    // Re-validated here, not just trusted from `config::parse_issuer`:
    // `discover` is the entry point every flow (login, refresh) calls, and
    // this check must hold regardless of which caller reaches it.
    security::require_secure(&issuer_url).context("issuer URL")?;

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
    // returned `issuer` to match what was requested. This alone is NOT
    // sufficient to trust the other endpoints below: `issuer` is just
    // another string field in the same JSON body an attacker who can
    // return this document at all could also control, and OIDC discovery
    // permits `authorization_endpoint`/`token_endpoint`/
    // `device_authorization_endpoint` to live on an entirely different
    // host. Matching `issuer` alone stops a discovery document from
    // impersonating a *different* realm; it does nothing to stop that
    // realm's own (compromised, misconfigured, or MITM'd) discovery
    // response from pointing its endpoints somewhere else -- see the
    // origin pinning below, which is what actually closes that gap.
    let discovered_issuer = metadata.issuer.trim_end_matches('/');
    if discovered_issuer != issuer {
        bail!(
            "OIDC discovery document at {url} claims issuer `{discovered_issuer}`, expected `{issuer}` -- refusing to trust it"
        );
    }

    require_same_origin(
        &issuer_url,
        &metadata.authorization_endpoint,
        "authorization_endpoint",
    )?;
    require_same_origin(&issuer_url, &metadata.token_endpoint, "token_endpoint")?;
    if let Some(device_endpoint) = &metadata.device_authorization_endpoint {
        require_same_origin(
            &issuer_url,
            device_endpoint,
            "device_authorization_endpoint",
        )?;
    }

    Ok(metadata)
}

/// Pins a discovered endpoint to the issuer's origin (scheme, host and
/// port) so a discovery response can't send the authorization, token, or
/// device-authorization request anywhere but where the issuer itself lives
/// -- see the comment in [`discover`] on why matching `issuer` alone
/// doesn't already guarantee this. `Url::origin` resolves default ports
/// (`https://x` == `https://x:443`), so this doesn't false-positive on an
/// endpoint that simply omits an explicit port the issuer spelled out, or
/// vice versa.
fn require_same_origin(issuer_url: &Url, endpoint: &str, field: &str) -> Result<()> {
    let endpoint_url = Url::parse(endpoint)
        .with_context(|| format!("parsing `{field}` from OIDC discovery document"))?;
    // Belt-and-braces: even if a same-origin endpoint URL somehow used a
    // scheme this binary otherwise wouldn't trust, reject it explicitly
    // rather than relying solely on the origin comparison below.
    security::require_secure(&endpoint_url)
        .with_context(|| format!("`{field}` from OIDC discovery document"))?;
    if endpoint_url.origin() != issuer_url.origin() {
        bail!(
            "OIDC discovery document's `{field}` is at a different origin than the issuer -- refusing to trust it"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // These exercise `require_same_origin` directly against synthetic
    // `Url`s rather than through a live server: the "downgrade to
    // plaintext on a non-loopback host" case needs an `https://` issuer,
    // and this test suite has no TLS test harness (deliberately -- see
    // `security.rs`'s module doc on why loopback plaintext is the only
    // carve-out, not a general "trust this" escape hatch). The end-to-end,
    // real-server version of the "different host" case lives in
    // `tests/tls_enforcement.rs`.

    fn url(raw: &str) -> Url {
        Url::parse(raw).expect("test URLs are well-formed")
    }

    #[test]
    fn matching_https_origin_is_accepted() {
        let issuer = url("https://auth.example.com/realms/platform");
        assert!(
            require_same_origin(
                &issuer,
                "https://auth.example.com/realms/platform/protocol/openid-connect/token",
                "token_endpoint",
            )
            .is_ok()
        );
    }

    #[test]
    fn a_different_host_is_rejected_even_when_issuer_string_would_have_matched() {
        // The exact shape of the attack this closes: `issuer` in the
        // discovery body could still equal the requested issuer (a
        // different check, already enforced above this one in `discover`)
        // while `token_endpoint` points somewhere else entirely.
        let issuer = url("https://auth.example.com/realms/platform");
        let error = require_same_origin(
            &issuer,
            "https://attacker.example/collect",
            "token_endpoint",
        )
        .expect_err("a token_endpoint on a different host must be rejected");
        assert!(error.to_string().contains("different origin"));
    }

    #[test]
    fn downgrading_to_plaintext_on_a_non_loopback_host_is_rejected() {
        // Same host as the issuer, but the scheme was downgraded --
        // exactly what a network attacker rewriting an in-flight discovery
        // response (or an operator's partial misconfiguration) looks like.
        let issuer = url("https://auth.example.com/realms/platform");
        let error = require_same_origin(
            &issuer,
            "http://auth.example.com/realms/platform/protocol/openid-connect/token",
            "token_endpoint",
        )
        .expect_err("a plaintext token_endpoint on a non-loopback host must be rejected");
        // `{:#}` renders the full `anyhow` context chain, not just the
        // outermost `with_context` message -- the "HTTPS" explanation
        // comes from `security::require_secure`, one level down.
        assert!(format!("{error:#}").to_ascii_lowercase().contains("https"));
    }

    #[test]
    fn matching_loopback_http_origin_is_still_accepted() {
        // Sanity check that origin pinning doesn't accidentally break the
        // carve-out the test suite's mock IdP depends on.
        let issuer = url("http://127.0.0.1:4181/realms/platform");
        assert!(
            require_same_origin(
                &issuer,
                "http://127.0.0.1:4181/protocol/token",
                "token_endpoint"
            )
            .is_ok()
        );
    }
}
