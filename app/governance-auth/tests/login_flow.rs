//! End-to-end: `governance-auth login` against a mock IdP, with the test
//! itself acting as "the browser" against the loopback redirect.

mod support;

use anyhow::{Context, Result};
use support::{
    harness::{Harness, wrong_state_action},
    mock_idp::{MockIdp, TokenBehavior},
};

#[tokio::test]
async fn login_via_browser_caches_a_session() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "issued-access-token".to_owned(),
        refresh_token: Some("issued-refresh-token".to_owned()),
        expires_in: 300,
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    let output = harness.login_via_browser().await?;

    assert!(
        output.status.success(),
        "login failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(output.stdout.is_empty(), "login must never write to stdout");

    let session_path = harness.session_path();
    let bytes =
        std::fs::read(&session_path).context("session cache file should exist after login")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&session_path)
            .context("stat session cache file")?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "session cache file must be mode 0600");
    }

    let session: serde_json::Value =
        serde_json::from_slice(&bytes).context("parse session cache")?;
    assert_eq!(session["access_token"], "issued-access-token");
    assert_eq!(session["refresh_token"], "issued-refresh-token");
    assert_eq!(idp.token_call_count()?, 1);
    Ok(())
}

#[tokio::test]
async fn login_rejects_a_forged_callback_state() -> Result<()> {
    // The mock IdP would happily issue a token if the client ever asked --
    // so a false pass here (accepting the tampered callback) is
    // detectable via `token_call_count` staying at 0, not just the exit
    // code.
    let idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "should-never-be-issued".to_owned(),
        refresh_token: None,
        expires_in: 300,
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    let output = harness
        .login_with_browser_action(wrong_state_action)
        .await?;

    assert!(
        !output.status.success(),
        "login must reject a callback whose `state` doesn't match the one it issued"
    );
    assert!(output.stdout.is_empty());
    assert_eq!(
        idp.token_call_count()?,
        0,
        "must never reach the token endpoint after a state mismatch"
    );
    assert!(
        !harness.session_path().exists(),
        "must not cache anything after a rejected callback"
    );
    Ok(())
}
