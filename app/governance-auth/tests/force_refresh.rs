//! `governance-auth refresh`: force a new access token now, and do it without
//! opening a hole in the fail-closed contract everything else here depends on.
//!
//! The four properties `oauth::refresh`'s module doc claims, one test each:
//! it really does go to the network when `token` would not, it prints no
//! credential, it refuses rather than logging in when there is nothing to
//! refresh from, and a server that says no leaves the cached session exactly
//! as it found it.

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

fn session(issuer: &str, access: &str, expires_at: u64) -> serde_json::Value {
    serde_json::json!({
        "issuer": issuer,
        "client_id": "test-client",
        "access_token": access,
        "refresh_token": "original-refresh-token",
        "expires_at": expires_at,
    })
}

/// The whole point of the command. `token` on this same cache returns the
/// cached value without a single request (`token_refresh.rs` pins that), so a
/// `token_call_count` of 1 here is the difference between "forced" and
/// "renamed `token`".
#[tokio::test]
async fn a_fresh_session_is_refreshed_anyway() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "forced-access-token".to_owned(),
        refresh_token: Some("rotated-refresh-token".to_owned()),
        expires_in: 300,
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    // An hour of life left: nothing about this session needs renewing.
    harness.seed_session(&session(
        &idp.base_url,
        "still-fresh-access-token",
        now_unix()? + 3600,
    ))?;

    let output = harness.run(&["refresh"]).await?;

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        idp.token_call_count()?,
        1,
        "refresh must go to the authorization server even on a fresh session"
    );

    let cached: serde_json::Value =
        serde_json::from_slice(&std::fs::read(harness.session_path()).context("read cache")?)
            .context("parse cache")?;
    assert_eq!(cached["access_token"], "forced-access-token");
    assert_eq!(
        cached["refresh_token"], "rotated-refresh-token",
        "a rotated refresh token must be persisted, or the next refresh fails"
    );
    Ok(())
}

/// Property 4 of the module doc. Wiring this into `apiKeyHelper` must break
/// loudly rather than half-work: a command that both forces a round trip AND
/// emits a credential would be a denial of service against the authorization
/// server that nobody would notice writing.
#[tokio::test]
async fn nothing_reaches_stdout() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "forced-access-token".to_owned(),
        refresh_token: None,
        expires_in: 300,
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    harness.seed_session(&session(&idp.base_url, "old", now_unix()? + 3600))?;

    let output = harness.run(&["refresh"]).await?;

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "",
        "refresh must never print a credential -- `token` is the only command that does"
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("forced-access-token"),
        "and it must not leak one onto stderr either"
    );
    Ok(())
}

/// Property 2. With no session there is nothing to refresh *from*, and the
/// answer is a non-zero exit naming `login` -- never a browser launch from a
/// command someone may have put on a timer.
#[tokio::test]
async fn an_empty_cache_refuses_instead_of_logging_in() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Fail {
        status: 500,
        error: "must_not_be_called",
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;

    let output = harness.run(&["refresh"]).await?;

    assert!(!output.status.success(), "an empty cache must fail closed");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no cached session"), "{stderr}");
    assert!(
        stderr.contains("login"),
        "must name the command that fixes it: {stderr}"
    );
    assert_eq!(
        idp.token_call_count()?,
        0,
        "nothing to refresh means nothing to ask for"
    );
    Ok(())
}

/// Property 3, and the one that would hurt most if it regressed. A refused
/// refresh must not be a logout: the developer asked for a *new* token, and
/// answering a network blip by destroying the working session they already had
/// would turn a diagnostic command into an outage.
#[tokio::test]
async fn a_refused_refresh_leaves_the_cached_session_untouched() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Fail {
        status: 400,
        error: "invalid_grant",
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    let seeded = session(
        &idp.base_url,
        "still-fresh-access-token",
        now_unix()? + 3600,
    );
    harness.seed_session(&seeded)?;
    let before = std::fs::read(harness.session_path()).context("read cache before")?;

    let output = harness.run(&["refresh"]).await?;

    assert!(!output.status.success(), "a refused refresh must fail");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    let after = std::fs::read(harness.session_path()).context("read cache after")?;
    assert_eq!(
        before, after,
        "the session file must be byte-identical after a failed refresh"
    );

    // And the session is still usable: `token` on the untouched cache still
    // answers, which is what "no worse off than before they asked" means.
    let token = harness.run(&["token"]).await?;
    assert!(token.status.success());
    assert_eq!(
        String::from_utf8_lossy(&token.stdout).trim(),
        "still-fresh-access-token"
    );
    Ok(())
}
