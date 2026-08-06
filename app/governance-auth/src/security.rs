//! The one transport-security predicate this binary trusts, applied at
//! three independent points so no single omission reopens the hole:
//!
//! 1. [`crate::config`] validates `--issuer` at CLI-parse time, before any
//!    network use.
//! 2. [`crate::oauth::discovery`] re-validates the issuer and origin-pins
//!    every endpoint the discovery document hands back
//!    (`authorization_endpoint`, `token_endpoint`,
//!    `device_authorization_endpoint`) against it, so a discovery response
//!    can't redirect credential-bearing requests to a different host.
//! 3. [`crate::main`] installs a custom [`reqwest::redirect::Policy`] that
//!    re-checks every hop of every redirect chain, so a same-origin HTTPS
//!    request can't be walked down to plaintext HTTP by a 3xx response.
//!
//! `governance-auth` is a public OAuth2 client (no client secret) handling
//! real user credentials -- authorization codes, PKCE verifiers, access and
//! refresh tokens -- over the network. Plaintext HTTP anywhere in that path,
//! whether from an operator's `--issuer http://…` typo or a network
//! attacker rewriting a response, lets those credentials be replayed
//! against the real Keycloak (see
//! docs/adr/0010-governance-auth-keycloak-oauth2-credential-helper.md).
//!
//! The one carve-out is loopback (`127.0.0.1`, `::1`, `localhost`): that
//! traffic never crosses a network, so there's no attacker positioned to
//! intercept it, and the test suite (`tests/support/mock_idp.rs`)
//! legitimately runs a plain-HTTP mock IdP there. This is a fixed,
//! structural exception baked into the predicate itself -- deliberately
//! not a configurable "allow insecure" flag or env var, which would be a
//! test double reachable from a production path (AGENTS.md).

use anyhow::{Result, bail};
use url::{Host, Url};

/// `127.0.0.1`, `::1`, or `localhost` -- the only hosts plaintext HTTP is
/// permitted against. Matches on the URL crate's already-parsed [`Host`]
/// rather than the raw string so an IPv6 literal's bracket syntax
/// (`[::1]`) doesn't need separate handling.
fn is_loopback(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(ip) => ip.is_loopback(),
        Host::Ipv6(ip) => ip.is_loopback(),
    }
}

/// Rejects any URL that is neither `https` nor loopback `http`. Every URL
/// this binary sends a credential-bearing request to -- or, for the
/// authorization endpoint, sends the user's browser to -- must pass this
/// before it's trusted.
pub fn require_secure(url: &Url) -> Result<()> {
    let scheme = url.scheme();
    let host = url.host();

    if scheme == "https" {
        return Ok(());
    }
    if scheme == "http" && host.as_ref().is_some_and(is_loopback) {
        return Ok(());
    }

    // Deliberately omit the path/query in this message: never log a full
    // URL, since a query string can carry a code/token in a malformed or
    // attacker-crafted response.
    bail!(
        "refusing a non-HTTPS URL (scheme `{scheme}`{}) -- only HTTPS is permitted; plaintext HTTP is allowed only against 127.0.0.1, ::1, or localhost",
        host.map(|h| format!(", host `{h}`")).unwrap_or_default(),
    );
}

/// The HTTP client's redirect policy (point 3 in this module's doc
/// comment): re-applies [`require_secure`] to every hop of every redirect
/// chain. This is what stops a same-origin HTTPS request from being walked
/// down to plaintext HTTP by a 3xx response -- [`require_secure`] alone,
/// called only on the URLs this binary constructs itself, wouldn't see a
/// redirect target a *server* chose.
pub fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| match require_secure(attempt.url()) {
        Ok(()) => attempt.follow(),
        Err(error) => attempt.error(error.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_is_always_allowed() {
        let url = Url::parse("https://auth.example.com/realms/platform").expect("valid url");
        assert!(require_secure(&url).is_ok());
    }

    #[test]
    fn plaintext_loopback_ipv4_is_allowed() {
        let url = Url::parse("http://127.0.0.1:4181/realms/platform").expect("valid url");
        assert!(require_secure(&url).is_ok());
    }

    #[test]
    fn plaintext_loopback_ipv6_is_allowed() {
        let url = Url::parse("http://[::1]:4181/realms/platform").expect("valid url");
        assert!(require_secure(&url).is_ok());
    }

    #[test]
    fn plaintext_localhost_is_allowed() {
        let url = Url::parse("http://localhost:4181/realms/platform").expect("valid url");
        assert!(require_secure(&url).is_ok());
    }

    #[test]
    fn plaintext_non_loopback_is_rejected() {
        let url = Url::parse("http://auth.example.com/realms/platform").expect("valid url");
        assert!(require_secure(&url).is_err());
    }

    #[test]
    fn plaintext_non_loopback_ip_is_rejected() {
        let url = Url::parse("http://203.0.113.5/realms/platform").expect("valid url");
        assert!(require_secure(&url).is_err());
    }
}
