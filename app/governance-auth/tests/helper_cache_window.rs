//! A token this binary prints must outlive the window the CALLER caches it
//! for, not merely the 30s clock skew.
//!
//! Measured in production on 2026-09-02: `otel headers` handed Claude Code a
//! token with 31s of life left, Claude Code cached it for
//! `CLAUDE_CODE_OTEL_HEADERS_HELPER_DEBOUNCE_MS` (240 000ms by default, a
//! value THIS binary writes), and the collector logged ~30 rejections per
//! 15-minute token -- with the session refresh landing three seconds AFTER
//! the expiry it was supposed to precede.
//!
//! `token` is the same shape: its output is cached for
//! `CLAUDE_CODE_API_KEY_HELPER_TTL_MS`, written from the same setting.

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

fn cached(harness: &Harness) -> Result<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(harness.session_path()).context("read session")?)
        .context("parse session")
}

/// (a) 200s of life left, and the caller will cache the answer for 240s.
/// The old rule (`expires_at > now + 30`) called this fresh and handed it
/// over; four minutes later Claude Code was still sending a token that had
/// been dead for 40s.
#[tokio::test]
async fn a_token_that_would_die_inside_the_callers_cache_window_is_refreshed() -> Result<()> {
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
        .run(&["--otel-headers-debounce-ms", "240000", "otel", "headers"])
        .await?;

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let headers: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parsing the emitted headers object")?;
    assert_eq!(
        headers["Authorization"], "Bearer refreshed-access-token",
        "the emitted token must be the refreshed one, not the near-dead cached one"
    );
    assert_eq!(
        idp.token_call_count()?,
        1,
        "exactly one refresh: the whole read-refresh-write runs under the file lock"
    );

    let remaining = i64::try_from(
        cached(&harness)?["expires_at"]
            .as_u64()
            .context("expires_at missing from the stored session")?,
    )? - i64::try_from(now_unix()?)?;
    assert!(
        remaining >= 270,
        "a handed-out token must outlive the 240s cache window plus the 30s skew, got {remaining}s"
    );
    Ok(())
}

/// (b) The correction must not become "refresh on every call": 600s of life
/// left comfortably outlives a 240s window plus the skew, so the cache hit
/// must still skip the network.
#[tokio::test]
async fn a_token_that_outlives_the_cache_window_is_not_refreshed() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Fail {
        status: 500,
        error: "must_not_be_called",
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    harness.seed_session(&serde_json::json!({
        "issuer": idp.base_url,
        "client_id": "test-client",
        "access_token": "comfortably-fresh",
        "refresh_token": "unused-refresh-token",
        "expires_at": now_unix()? + 600,
        "lifetime_secs": 900,
    }))?;

    let output = harness
        .run(&["--otel-headers-debounce-ms", "240000", "otel", "headers"])
        .await?;

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        idp.token_call_count()?,
        0,
        "600s > 240s + 30s, so this must stay a pure cache hit"
    );
    Ok(())
}

/// (d) The degenerate case: a cache window at least as long as the token
/// lifetime would make every freshly minted token "stale" on arrival, so the
/// requirement is capped at half the observed lifetime and the operator is
/// told on STDERR -- never stdout, which carries the credential.
#[tokio::test]
async fn a_cache_window_longer_than_the_token_lifetime_is_capped_not_looped() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Fail {
        status: 500,
        error: "must_not_be_called",
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    harness.seed_session(&serde_json::json!({
        "issuer": idp.base_url,
        "client_id": "test-client",
        "access_token": "short-lived-but-usable",
        "refresh_token": "unused-refresh-token",
        "expires_at": now_unix()? + 200,
        // Shorter than the 600s window below: uncapped, the requirement
        // (630s) exceeds anything this authorization server can ever mint.
        "lifetime_secs": 300,
    }))?;

    let output = harness
        .run(&["--otel-headers-debounce-ms", "600000", "otel", "headers"])
        .await?;

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "stderr={stderr}");
    assert_eq!(
        idp.token_call_count()?,
        0,
        "the cap is what stops a refresh on literally every invocation"
    );
    assert!(
        stderr.contains("otel-headers-debounce-ms"),
        "the warning must name the setting to lower, got stderr={stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        !stdout.contains("warning"),
        "the warning must never reach stdout -- stdout is the credential, got stdout={stdout}"
    );
    let headers: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parsing the emitted headers object")?;
    assert_eq!(headers["Authorization"], "Bearer short-lived-but-usable");
    Ok(())
}
