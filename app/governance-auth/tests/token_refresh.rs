//! `governance-auth token`: cache-hit fast path, refresh when the cached
//! session cannot cover the caller's cache window (the rule itself lives in
//! `tests/helper_cache_window.rs`), refresh-token rotation persistence.

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
async fn a_fresh_cached_token_is_returned_without_a_network_call() -> Result<()> {
    // Configured to fail loudly if it's ever actually called -- proves the
    // cache hit is real, not just "happened to still work".
    let idp = MockIdp::start(TokenBehavior::Fail {
        status: 500,
        error: "must_not_be_called",
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    harness.seed_session(&serde_json::json!({
        "issuer": idp.base_url,
        "client_id": "test-client",
        "access_token": "still-fresh-access-token",
        "refresh_token": "unused-refresh-token",
        "expires_at": now_unix()? + 3600,
    }))?;

    let output = harness.run(&["token"]).await?;

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "still-fresh-access-token"
    );
    assert_eq!(
        idp.token_call_count()?,
        0,
        "cache hit must skip the network"
    );
    Ok(())
}

#[tokio::test]
async fn a_near_expiry_token_is_refreshed_and_the_rotated_refresh_token_persisted() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "refreshed-access-token".to_owned(),
        refresh_token: Some("rotated-refresh-token".to_owned()),
        expires_in: 300,
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    harness.seed_session(&serde_json::json!({
        "issuer": idp.base_url,
        "client_id": "test-client",
        "access_token": "stale-access-token",
        "refresh_token": "original-refresh-token",
        "expires_at": now_unix()? + 5, // inside every margin

    }))?;

    let output = harness.run(&["token"]).await?;

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "refreshed-access-token"
    );
    assert_eq!(idp.token_call_count()?, 1);

    let cached: serde_json::Value =
        serde_json::from_slice(&std::fs::read(harness.session_path()).context("read cache")?)
            .context("parse cache")?;
    assert_eq!(cached["access_token"], "refreshed-access-token");
    assert_eq!(cached["refresh_token"], "rotated-refresh-token");
    Ok(())
}

#[tokio::test]
async fn a_refresh_response_without_a_new_refresh_token_keeps_the_old_one() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "refreshed-access-token".to_owned(),
        refresh_token: None,
        expires_in: 300,
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    harness.seed_session(&serde_json::json!({
        "issuer": idp.base_url,
        "client_id": "test-client",
        "access_token": "stale-access-token",
        "refresh_token": "keep-this-refresh-token",
        "expires_at": now_unix()?,
    }))?;

    let output = harness.run(&["token"]).await?;
    assert!(output.status.success());

    let cached: serde_json::Value =
        serde_json::from_slice(&std::fs::read(harness.session_path()).context("read cache")?)
            .context("parse cache")?;
    assert_eq!(cached["refresh_token"], "keep-this-refresh-token");
    Ok(())
}

/// `token` feeds `apiKeyHelper`, whose output Claude Code caches for
/// `CLAUDE_CODE_API_KEY_HELPER_TTL_MS` -- the same setting. Same rule, or the
/// two commands drift.
#[tokio::test]
async fn the_token_command_obeys_the_same_cache_window_as_otel_headers() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "refreshed-access-token".to_owned(),
        refresh_token: Some("rotated-refresh-token".to_owned()),
        expires_in: 900,
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    harness.seed_session(&serde_json::json!({
        "issuer": idp.base_url,
        "client_id": "test-client",
        "access_token": "outlived-by-the-cache-window",
        "refresh_token": "original-refresh-token",
        "expires_at": now_unix()? + 200,
        "lifetime_secs": 900,
    }))?;

    let output = harness
        .run(&["--otel-headers-debounce-ms", "240000", "token"])
        .await?;

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "refreshed-access-token"
    );
    assert_eq!(idp.token_call_count()?, 1);
    Ok(())
}

/// A session from an older build carries no `lifetime_secs`: still usable,
/// still refreshed when it cannot outlive the caller's window. The cap is
/// unavailable for that one call; the refreshed session then records one.
#[tokio::test]
async fn a_legacy_session_without_a_recorded_lifetime_still_refreshes() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "refreshed-access-token".to_owned(),
        refresh_token: Some("rotated-refresh-token".to_owned()),
        expires_in: 900,
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    harness.seed_session(&serde_json::json!({
        "issuer": idp.base_url,
        "client_id": "test-client",
        "access_token": "outlived-by-the-cache-window",
        "refresh_token": "original-refresh-token",
        "expires_at": now_unix()? + 200,
    }))?;

    let output = harness
        .run(&["--otel-headers-debounce-ms", "240000", "otel", "headers"])
        .await?;

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(idp.token_call_count()?, 1);
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(harness.session_path()).context("read session")?)
            .context("parse session")?;
    assert_eq!(
        stored["lifetime_secs"], 900,
        "the refreshed session must record the lifetime the cap needs next time"
    );
    Ok(())
}
