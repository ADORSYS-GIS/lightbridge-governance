//! Proves the transport-security fixes: a non-loopback plaintext issuer is
//! rejected before any network call, and a discovery document whose
//! `issuer` matches but whose `token_endpoint` is off-origin is rejected
//! before the authorization code and PKCE verifier are ever sent there.
//! See `src/security.rs` and the comment in `src/oauth/discovery.rs` on
//! `discover` for why matching `issuer` alone isn't sufficient.

mod support;

use anyhow::{Context, Result};
use support::{
    harness::Harness,
    mock_idp::{DiscoveryOverrides, MockIdp, TokenBehavior},
};

#[tokio::test]
async fn token_rejects_a_non_loopback_plaintext_issuer() -> Result<()> {
    // Never reachable, and doesn't need to be: config-time validation must
    // reject this before governance-auth ever tries to resolve or connect
    // to it -- a network attacker who could intercept this issuer's traffic
    // is exactly the case this closes.
    let harness = Harness::new("http://auth.example.com")?;

    let output = harness.run(&["token"]).await?;

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "must never print a token on failure"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_ascii_lowercase().contains("https"),
        "stderr should explain the HTTPS requirement, got: {stderr}"
    );
    Ok(())
}

#[tokio::test]
async fn login_never_posts_the_code_to_an_off_origin_token_endpoint() -> Result<()> {
    // The "attacker": a second mock server that would happily mint a token
    // if the authorization-code exchange ever reached it -- exactly the
    // concrete attack this fix closes (a compromised/misconfigured/MITM'd
    // discovery response redirecting the code + PKCE verifier to a
    // different endpoint the real Keycloak never authorized).
    let attacker = MockIdp::start(TokenBehavior::Succeed {
        access_token: "attacker-issued-access-token".to_owned(),
        refresh_token: None,
        expires_in: 300,
    })
    .await?;

    // The "legit" IdP: its `issuer` matches what's requested (so the RFC
    // 8414 §3.1.2 string check alone would pass it), but its discovery
    // document advertises the attacker's `token_endpoint`.
    let idp = MockIdp::start_with_discovery_overrides(
        TokenBehavior::Fail {
            status: 500,
            error: "must_not_be_called",
        },
        DiscoveryOverrides {
            token_endpoint: Some(format!("{}/token", attacker.base_url)),
            ..Default::default()
        },
    )
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    let output = harness.login_via_browser().await?;

    assert!(
        !output.status.success(),
        "login must reject a discovery document whose token_endpoint is off-origin: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(output.stdout.is_empty());
    assert_eq!(
        attacker.token_call_count()?,
        0,
        "the authorization code and PKCE verifier must never reach the off-origin endpoint"
    );
    assert!(
        !harness.session_path().exists(),
        "must not cache anything after a rejected discovery document"
    );
    Ok(())
}

/// A minimal server whose only route answers the discovery request with an
/// HTTP redirect to a non-loopback plaintext URL -- exercising the HTTP
/// client's redirect policy specifically (`main.rs`'s
/// `security::redirect_policy`), independent of `oauth::discovery`'s
/// origin pinning: the redirect is followed (or refused) entirely inside
/// `reqwest`, before a JSON discovery body -- with an `issuer` field for
/// `require_same_origin` to check -- ever exists.
async fn start_downgrade_redirect_server() -> Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding hostile redirect listener")?;
    let addr = listener
        .local_addr()
        .context("reading hostile redirect listener address")?;
    let base_url = format!("http://{addr}");

    let router = axum::Router::new().route(
        "/.well-known/openid-configuration",
        axum::routing::get(|| async {
            // 198.51.100.0/24 (RFC 5737 TEST-NET-2) is reserved for
            // documentation/examples and never routes anywhere -- if the
            // client actually tried to connect here, that alone would
            // prove the redirect was followed, which the assertions below
            // wait long enough to rule out isn't just "still connecting".
            axum::response::Redirect::temporary("http://198.51.100.1/evil")
        }),
    );

    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    Ok(base_url)
}

#[tokio::test]
async fn client_refuses_to_follow_a_discovery_redirect_to_plaintext() -> Result<()> {
    let base_url = start_downgrade_redirect_server().await?;
    let harness = Harness::new(&base_url)?;

    // `login` reaches `discovery::discover` before any browser step, so a
    // plain (non-browser-interacting) run is enough here.
    let output = harness.run(&["login"]).await?;

    assert!(
        !output.status.success(),
        "must refuse to follow a redirect to a non-loopback plaintext URL"
    );
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_ascii_lowercase().contains("https"),
        "stderr should explain the HTTPS requirement, got: {stderr}"
    );
    Ok(())
}
