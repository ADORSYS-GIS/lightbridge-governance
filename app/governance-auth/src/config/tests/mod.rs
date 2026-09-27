//! Tests for [`super`]. Split into its own file (issue #175) rather than
//! raising the already-grandfathered LoC ceiling -- the same move
//! `otel/tests.rs` made, applied to the whole `mod tests` block.

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

/// `--exchange-token-endpoint` is explicitly not an issuer -- the error
/// message for an unparseable value must say "endpoint", not "issuer",
/// so an operator who typo'd this flag isn't told the wrong flag is
/// wrong. Regression test for the message `parse_issuer` used to
/// produce here (it was reused verbatim, hardcoding "issuer" for both).
#[test]
fn exchange_token_endpoint_parse_error_names_endpoint_not_issuer() {
    let error = parse_exchange_token_endpoint("not a url")
        .expect_err("an unparseable exchange-token-endpoint value must be rejected");
    assert!(
        error.contains("endpoint"),
        "error should say 'endpoint', not 'issuer' -- this flag is a token endpoint, not an \
         issuer, got: {error}"
    );
    assert!(
        !error.contains("issuer"),
        "error should not call this value an issuer, got: {error}"
    );
}

/// ADR-0012 Decision 2's five layers, proved pairwise: flag beats env,
/// env beats per-user file, per-user file beats machine-wide file,
/// machine-wide file beats the compiled default.
///
/// Every test below drives [`OauthConfigArgs::resolve_with_paths`]
/// directly with temp-file paths for the two file layers, rather than
/// going through `resolve()`'s real `/etc/governance-auth/config.toml`
/// and `$HOME`-derived per-user path -- that's what "the paths are
/// injectable" buys: these tests never touch the real filesystem
/// locations, so they're hermetic and safe to run in parallel with
/// every other test in this crate, including ones that touch a real
/// `$HOME` through the subprocess harness in `tests/`.
mod precedence;

/// `--callback-port`/`--callback-bind` (issue #364): same five-layer proof,
/// its own file rather than growing `precedence` further.
mod callback;
