//! OIDC discovery (`/.well-known/openid-configuration`). Endpoints are never
//! hand-derived from the issuer URL -- discovery is what lets this binary
//! work against any Keycloak realm without a code change if paths move.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::{cache, security};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcMetadata {
    pub issuer: String,
    /// `Option`, not `String`, and this is load-bearing rather than defensive.
    ///
    /// OIDC Discovery 1.0 §3 marks this REQUIRED, but only for providers that
    /// actually serve an authorization endpoint. `lightbridge-authz` serves
    /// none -- it has no `/authorize` route and never redirects a user-agent,
    /// so it omits the field deliberately and correctly (see its `signing.rs`,
    /// "Authorization endpoint -- never advertised, in either state").
    ///
    /// Requiring it here meant `--exchange-issuer` could not discover that
    /// server AT ALL, failing with a raw serde message
    /// (`missing field 'authorization_endpoint'`) before any request was made.
    /// Token exchange is a direct POST to the token endpoint and never touches
    /// this field, so demanding it broke a flow that does not need it.
    ///
    /// Only the authorization-code flow requires it; `authcode.rs` produces a
    /// clear error when it is absent.
    pub authorization_endpoint: Option<String>,
    pub token_endpoint: String,
    pub device_authorization_endpoint: Option<String>,
    /// RFC 7009. `Option` because it is not in the OIDC Discovery core spec
    /// -- Keycloak advertises it, but a `logout` that assumed it exists
    /// would break against an authorization server that doesn't.
    pub revocation_endpoint: Option<String>,
}

/// How long a cached discovery document is reused. Endpoint URLs are close
/// to static for the life of a realm, so this trades a rare extra round trip
/// after a move against removing one on EVERY token refresh.
const DISCOVERY_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Cached form of a validated discovery document.
///
/// ⚠️ The cache is a LATENCY optimisation, never a trust shortcut. Every
/// validation in [`discover`] -- issuer match and per-endpoint origin
/// pinning -- is re-run on the cached copy in [`validate`] before it is
/// returned. A cache file is an attacker-writable input (it lives in the
/// user's cache directory), so treating it as pre-trusted would let anyone
/// who can write that file redirect the token and revocation endpoints.
#[derive(Debug, Serialize, Deserialize)]
struct CachedDiscovery {
    fetched_at: u64,
    metadata: OidcMetadata,
}

fn discovery_cache_path(issuer: &str) -> Result<std::path::PathBuf> {
    let mut hasher = Sha256::new();
    hasher.update(issuer.as_bytes());
    Ok(cache::cache_dir()?.join(format!("discovery-{}.json", hex::encode(hasher.finalize()))))
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Reads a still-fresh cached document. Any failure -- missing, unreadable,
/// unparseable, stale, or failing revalidation -- yields `None` so the
/// caller refetches. A bad cache must never be fatal.
fn load_cached(issuer: &str, issuer_url: &Url) -> Option<OidcMetadata> {
    let path = discovery_cache_path(issuer).ok()?;
    let bytes = std::fs::read(path).ok()?;
    let cached: CachedDiscovery = serde_json::from_slice(&bytes).ok()?;
    if now_unix().saturating_sub(cached.fetched_at) > DISCOVERY_TTL.as_secs() {
        return None;
    }
    validate(issuer, issuer_url, &cached.metadata).ok()?;
    Some(cached.metadata)
}

fn store_cached(issuer: &str, metadata: &OidcMetadata) {
    // Best-effort: a read-only or missing cache directory must not break
    // authentication, it just means no caching.
    let Ok(path) = discovery_cache_path(issuer) else {
        return;
    };
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let record = CachedDiscovery {
        fetched_at: now_unix(),
        metadata: metadata.clone(),
    };
    if let Ok(bytes) = serde_json::to_vec(&record) {
        let _ = std::fs::write(path, bytes);
    }
}

pub async fn discover(http: &reqwest::Client, issuer: &str) -> Result<OidcMetadata> {
    let issuer = issuer.trim_end_matches('/');
    let issuer_url = Url::parse(issuer).with_context(|| "parsing the configured issuer URL")?;
    // Re-validated here, not just trusted from `config::parse_issuer` -- see
    // the note below; this must hold before the cache is consulted too.
    security::require_secure(&issuer_url).context("issuer URL")?;

    // Served from cache when fresh. This is on the hot path: `token` and
    // `otel headers` are spawned every 240s by two clients, and each one
    // previously paid a full discovery round trip before the refresh.
    if let Some(metadata) = load_cached(issuer, &issuer_url) {
        return Ok(metadata);
    }

    discover_uncached(http, issuer, &issuer_url).await
}

async fn discover_uncached(
    http: &reqwest::Client,
    issuer: &str,
    issuer_url: &Url,
) -> Result<OidcMetadata> {
    let issuer_url = issuer_url.clone();

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

    validate(issuer, &issuer_url, &metadata)?;
    store_cached(issuer, &metadata);
    Ok(metadata)
}

/// Every check that must hold before an endpoint from this document is used.
///
/// Split out of [`discover`] so the CACHED path runs byte-for-byte the same
/// validation as the freshly-fetched one -- see [`CachedDiscovery`] for why
/// a cache file cannot be treated as pre-trusted.
fn validate(issuer: &str, issuer_url: &Url, metadata: &OidcMetadata) -> Result<()> {
    // OIDC Discovery (RFC 8414 §3.1.2 / OIDC Discovery 4.3) requires the
    // returned `issuer` to match what was requested. This alone is NOT
    // sufficient to trust the other endpoints: `issuer` is just another
    // string field in the same JSON body an attacker who can return this
    // document at all could also control, and OIDC discovery permits the
    // endpoints to live on an entirely different host. Matching `issuer`
    // stops a document impersonating a *different* realm; it does nothing
    // to stop that realm's own (compromised, misconfigured, or MITM'd)
    // response pointing its endpoints elsewhere -- the origin pinning below
    // is what actually closes that gap.
    let discovered_issuer = metadata.issuer.trim_end_matches('/');
    if discovered_issuer != issuer {
        bail!(
            "OIDC discovery document claims issuer `{discovered_issuer}`, expected `{issuer}` -- refusing to trust it"
        );
    }

    // Pinned WHEN PRESENT. Absent is a legitimate state (a server that serves
    // no authorization endpoint, e.g. lightbridge-authz), not a reason to skip
    // the check when it IS advertised -- an omitted field must never become a
    // way to dodge origin pinning.
    if let Some(authorization_endpoint) = &metadata.authorization_endpoint {
        require_same_origin(issuer_url, authorization_endpoint, "authorization_endpoint")?;
    }
    require_same_origin(issuer_url, &metadata.token_endpoint, "token_endpoint")?;
    if let Some(device_endpoint) = &metadata.device_authorization_endpoint {
        require_same_origin(issuer_url, device_endpoint, "device_authorization_endpoint")?;
    }
    // ⚠️ Load-bearing, not symmetry-for-its-own-sake: `logout` POSTs the
    // REFRESH TOKEN to this endpoint. Without pinning, a document naming an
    // attacker-controlled host would turn `logout` into credential
    // exfiltration -- the one flow whose whole purpose is to destroy it.
    if let Some(revocation_endpoint) = &metadata.revocation_endpoint {
        require_same_origin(issuer_url, revocation_endpoint, "revocation_endpoint")?;
    }
    Ok(())
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
