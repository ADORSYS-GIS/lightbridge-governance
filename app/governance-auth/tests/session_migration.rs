//! The session lives in STATE, not CACHE, and a session written by an older
//! build is migrated rather than abandoned.
//!
//! Why this has its own test file: the failure this guards against is
//! silent and badly timed. A refresh token under `~/Library/Caches` (macOS
//! purges it under disk pressure) or `~/.cache` (every disk-cleanup tool,
//! and container image layers that prune it) means `token` fails closed
//! INSIDE a running session -- and per `docs/integrations/ai-client-flows.md`
//! Codex responds by proceeding UNAUTHENTICATED rather than stopping.

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
async fn a_session_written_by_an_older_build_is_migrated_out_of_the_cache() -> Result<()> {
    // Fails loudly if the network is touched: proves the migrated session
    // was actually USED, not silently discarded and re-fetched.
    let idp = MockIdp::start(TokenBehavior::Fail {
        status: 500,
        error: "must_not_be_called",
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    harness.seed_legacy_session(&serde_json::json!({
        "issuer": idp.base_url,
        "client_id": "test-client",
        "access_token": "token-from-the-old-location",
        "refresh_token": "unused-refresh-token",
        "expires_at": now_unix()? + 3600,
    }))?;

    let output = harness.run(&["token"]).await?;

    assert!(
        output.status.success(),
        "a session at the legacy path must still work: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "token-from-the-old-location",
    );
    assert!(
        harness.session_path().is_file(),
        "the session must have moved to the state path"
    );
    assert!(
        !harness.legacy_session_path().exists(),
        "the legacy copy must be removed -- leaving it behind leaves a \
         refresh token in a directory the OS may purge, AND a second copy \
         that `logout` would have to know about"
    );
    Ok(())
}

#[tokio::test]
async fn logout_clears_both_locations() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Fail {
        status: 500,
        error: "unused",
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    let session = serde_json::json!({
        "issuer": idp.base_url,
        "client_id": "test-client",
        "access_token": "access",
        "refresh_token": "refresh",
        "expires_at": now_unix()? + 3600,
    });
    // Both populated at once -- the state the migration path would produce
    // if it had failed partway, and exactly the case where clearing only
    // one leaves a usable credential behind while reporting success.
    harness.seed_session(&session)?;
    harness.seed_legacy_session(&session)?;

    let output = harness.run(&["logout"]).await?;

    assert!(
        output.status.success(),
        "logout must succeed even when revocation cannot reach the server: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !harness.session_path().exists(),
        "state copy must be gone after logout"
    );
    assert!(
        !harness.legacy_session_path().exists(),
        "legacy copy must ALSO be gone -- a logout that reports success and \
         leaves a live refresh token on disk is worse than one that fails"
    );
    Ok(())
}
