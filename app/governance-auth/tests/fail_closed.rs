//! `governance-auth token` must fail closed: non-zero exit, nothing on
//! stdout, no interactive browser -- whenever it can't produce a genuinely
//! valid token. This is the property `apiKeyHelper`/`auth.command` depend
//! on: a bad exit here is a clear signal to the caller, a fabricated or
//! stale token on stdout is a silent authorization bypass.

mod support;

use anyhow::{Context, Result};
use support::{
    harness::Harness,
    mock_idp::{MockIdp, TokenBehavior},
};

fn now_unix() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_secs())
}

#[tokio::test]
async fn token_fails_closed_with_no_cached_session() -> Result<()> {
    // Never reachable: `token` must bail before ever calling discovery.
    // `.invalid` is guaranteed (RFC 2606) never to resolve; `https://` so
    // this is rejected for being unreachable, not for being insecure --
    // see `oauth::discovery`/`config::parse_issuer` for the latter.
    let harness = Harness::new("https://unreachable.invalid.example")?;

    let output = harness.run(&["token"]).await?;

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "must never print a token on failure"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("login"),
        "stderr should point the user at `governance-auth login`"
    );
    Ok(())
}

#[tokio::test]
async fn token_fails_closed_when_expired_and_no_refresh_token() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&serde_json::json!({
        "issuer": "https://unreachable.invalid.example",
        "client_id": "test-client",
        "access_token": "expired-access-token",
        "refresh_token": null,
        "expires_at": now_unix()?.saturating_sub(3600),
    }))?;

    let output = harness.run(&["token"]).await?;

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    Ok(())
}

#[tokio::test]
async fn token_fails_closed_when_the_idp_rejects_the_refresh() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Fail {
        status: 400,
        error: "invalid_grant",
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    let seeded = serde_json::json!({
        "issuer": idp.base_url,
        "client_id": "test-client",
        "access_token": "stale-access-token",
        "refresh_token": "revoked-refresh-token",
        "expires_at": now_unix()?,
    });
    harness.seed_session(&seeded)?;

    let output = harness.run(&["token"]).await?;

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "must not print the stale access token when refresh fails"
    );

    let on_disk: serde_json::Value =
        serde_json::from_slice(&std::fs::read(harness.session_path()).context("read cache")?)
            .context("parse cache")?;
    assert_eq!(
        on_disk, seeded,
        "a failed refresh must not overwrite the existing cache entry"
    );
    Ok(())
}
