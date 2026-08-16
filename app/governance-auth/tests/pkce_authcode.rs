//! PKCE (RFC 7636) is unconditional on the loopback authorization-code flow
//! -- ALWAYS sent, never a flag, never optional. RFC 8252 / OAuth 2.1
//! require it for public clients (this binary ships with no client secret),
//! and `oauth::authcode`'s module doc says so explicitly. This is the
//! regression guard: it asserts the real authorize URL a real client run
//! actually builds carries `code_challenge`/`code_challenge_method=S256`,
//! and that the verifier later sent to the token endpoint is the one that
//! hashes to it -- not just that the flow completes.
//!
//! (`tests/device_flow.rs` proves the same property for the device-code
//! flow, which already sends PKCE unconditionally too.)

mod support;

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use support::{
    harness::{Harness, correct_state_action},
    mock_idp::{MockIdp, TokenBehavior},
};

#[tokio::test]
async fn login_via_browser_always_sends_pkce() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "issued-access-token".to_owned(),
        refresh_token: Some("issued-refresh-token".to_owned()),
        expires_in: 300,
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    let (output, authorize_url) = harness
        .login_capturing_authorize_url(correct_state_action)
        .await?;

    assert!(
        output.status.success(),
        "login failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let parsed = url::Url::parse(&authorize_url).context("parsing the authorize url")?;
    let mut code_challenge = None;
    let mut code_challenge_method = None;
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "code_challenge" => code_challenge = Some(value.into_owned()),
            "code_challenge_method" => code_challenge_method = Some(value.into_owned()),
            _ => {}
        }
    }

    assert_eq!(
        code_challenge_method.as_deref(),
        Some("S256"),
        "the authorize URL must always advertise S256, never omit it or send it \
         half-heartedly -- got: {authorize_url}"
    );
    let code_challenge = code_challenge.context(format!(
        "the authorize URL carried no code_challenge at all -- got: {authorize_url}"
    ))?;
    assert!(
        !code_challenge.is_empty(),
        "code_challenge must not be empty"
    );

    Ok(())
}

/// Stronger than the URL-shape check above: recomputes S256(verifier) from
/// what the client actually POSTed to the token endpoint and checks it
/// against the `code_challenge` the SAME run put on the authorize URL --
/// ruling out two unrelated PKCE pairs slipping through undetected (the
/// same cross-check `tests/device_flow.rs` already does for the device
/// flow).
#[tokio::test]
async fn the_verifier_sent_to_the_token_endpoint_matches_the_challenge_on_the_authorize_url()
-> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "issued-access-token".to_owned(),
        refresh_token: Some("issued-refresh-token".to_owned()),
        expires_in: 300,
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    let (output, authorize_url) = harness
        .login_capturing_authorize_url(correct_state_action)
        .await?;
    assert!(output.status.success());

    let parsed = url::Url::parse(&authorize_url).context("parsing the authorize url")?;
    let challenge_sent = parsed
        .query_pairs()
        .find(|(key, _)| key == "code_challenge")
        .map(|(_, value)| value.into_owned())
        .context("authorize url carried no code_challenge")?;

    let verifier_sent = idp
        .last_authcode_code_verifier()?
        .context("token endpoint request carried no code_verifier at all")?;

    let mut hasher = Sha256::new();
    hasher.update(verifier_sent.as_bytes());
    let recomputed_challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());
    assert_eq!(
        recomputed_challenge, challenge_sent,
        "the code_verifier sent to the token endpoint must hash (S256) to the code_challenge \
         sent on the authorize URL"
    );
    Ok(())
}
