//! End-to-end: `governance-auth login --device-code` against a mock IdP.
//!
//! The decisive check here isn't "the flow completes" -- a mock that
//! ignores its own request bodies would let that pass even if the client
//! sent no PKCE params at all. It's that the client actually sends
//! `code_challenge`/`code_challenge_method=S256` on the device-authorization
//! request and the *matching* `code_verifier` on the token poll, because
//! this org's real IdP (Keycloak, PKCE required on the client) rejects the
//! device-authorization request outright without them
//! (`invalid_request: Missing parameter: code_challenge_method`) --
//! confirmed against the real endpoint before this fix existed.

mod support;

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use support::{
    harness::Harness,
    mock_idp::{MockIdp, TokenBehavior},
};

#[tokio::test]
async fn login_via_device_code_sends_pkce_and_caches_a_session() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "issued-access-token".to_owned(),
        refresh_token: Some("issued-refresh-token".to_owned()),
        expires_in: 300,
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    let output = harness.run(&["login", "--device-code"]).await?;

    assert!(
        output.status.success(),
        "device-code login failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(output.stdout.is_empty(), "login must never write to stdout");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("MOCK-CODE"),
        "must print the verification code for the human to enter, got: {stderr}"
    );

    assert_eq!(idp.device_call_count()?, 1);
    assert_eq!(
        idp.last_device_code_challenge_method()?.as_deref(),
        Some("S256"),
        "device-authorization request must advertise S256, not send PKCE half-heartedly"
    );

    let challenge_sent = idp
        .last_device_code_challenge()?
        .context("device-authorization request carried no code_challenge at all")?;
    let verifier_sent = idp
        .last_token_code_verifier()?
        .context("token poll carried no code_verifier at all")?;

    // Recompute S256(verifier) exactly as the server would and check it
    // against what was actually sent on the device-authorization step --
    // this is what rules out a challenge and verifier from two unrelated
    // PKCE pairs slipping through undetected.
    let mut hasher = Sha256::new();
    hasher.update(verifier_sent.as_bytes());
    let recomputed_challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());
    assert_eq!(
        recomputed_challenge, challenge_sent,
        "the code_verifier sent to the token endpoint must hash (S256) to the \
         code_challenge sent to the device-authorization endpoint"
    );

    let session_path = harness.session_path();
    let bytes =
        std::fs::read(&session_path).context("session cache file should exist after login")?;
    let session: serde_json::Value =
        serde_json::from_slice(&bytes).context("parse session cache")?;
    assert_eq!(session["access_token"], "issued-access-token");
    assert_eq!(session["refresh_token"], "issued-refresh-token");
    assert_eq!(idp.token_call_count()?, 1);
    Ok(())
}
