//! RFC 8693 token exchange (issue #140): OFF by default, opt-in only, and
//! FAIL CLOSED -- `token`/`otel-headers` must never fall back to the raw
//! upstream token when exchange is enabled but fails. Uses a SECOND
//! `MockIdp` instance as the exchange authorization server, entirely
//! independent from the primary (upstream) one -- proving the "authenticate
//! at A, present credentials minted by B" pair is genuinely two different
//! servers, not the same mock answering twice.

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

/// A session that's fresh enough that `current_session` never needs to
/// refresh it -- keeps these tests focused on the exchange step, not the
/// primary token's refresh cycle (already covered by `tests/token_refresh.rs`).
fn seed_fresh_session(harness: &Harness, access_token: &str) -> Result<()> {
    harness.seed_session(&serde_json::json!({
        "issuer": harness.issuer(),
        "client_id": harness.client_id(),
        "access_token": access_token,
        "refresh_token": "refresh-token",
        "expires_at": now_unix()?.saturating_add(3600),
    }))
}

#[tokio::test]
async fn token_exchange_off_by_default_emits_the_raw_upstream_token() -> Result<()> {
    // Never reachable: with exchange off, nothing should ever call out to an
    // exchange endpoint at all. `.invalid` (RFC 2606) can never resolve, so
    // a call here would fail loudly rather than silently succeeding.
    let harness = Harness::new("https://unreachable.invalid.example")?;
    seed_fresh_session(&harness, "upstream-access-token")?;

    let output = harness.run(&["token"]).await?;

    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "upstream-access-token",
        "with token exchange off (the default), `token` must print the raw upstream token \
         unchanged"
    );
    Ok(())
}

#[tokio::test]
async fn token_exchange_enabled_emits_the_exchanged_token_not_the_upstream_one() -> Result<()> {
    let exchange_idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "exchanged-token".to_owned(),
        refresh_token: None,
        expires_in: 900,
    })
    .await?;

    let harness = Harness::new("https://unreachable.invalid.example")?;
    seed_fresh_session(&harness, "upstream-access-token")?;

    let output = harness
        .run(&[
            "token",
            "--token-exchange",
            "--exchange-token-endpoint",
            &format!("{}/token", exchange_idp.base_url),
            "--exchange-client-id",
            "exchange-cli",
        ])
        .await?;

    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "exchanged-token",
        "with token exchange on, `token` must emit the EXCHANGED token, not the upstream one"
    );
    assert_ne!(
        stdout.trim(),
        "upstream-access-token",
        "must never emit the un-exchanged upstream token when exchange is enabled"
    );
    assert_eq!(
        exchange_idp.token_call_count()?,
        1,
        "the exchange endpoint must actually have been called"
    );
    Ok(())
}

/// The `--exchange-issuer` discovery path -- what the RUNBOOK's canonical
/// example uses -- driven end to end against a mock, instead of the
/// `--exchange-token-endpoint` shortcut every other test in this file uses.
/// `--exchange-token-endpoint` skips discovery entirely
/// (`ExchangeTokenEndpoint::Explicit`); this proves the OTHER branch,
/// `ExchangeTokenEndpoint::Issuer`, actually resolves a token endpoint via a
/// real OIDC discovery round trip and uses it, not just that it compiles.
#[tokio::test]
async fn token_exchange_via_exchange_issuer_discovery_emits_the_exchanged_token() -> Result<()> {
    let exchange_idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "exchanged-token".to_owned(),
        refresh_token: None,
        expires_in: 900,
    })
    .await?;

    let harness = Harness::new("https://unreachable.invalid.example")?;
    seed_fresh_session(&harness, "upstream-access-token")?;

    let output = harness
        .run(&[
            "token",
            "--token-exchange",
            "--exchange-issuer",
            &exchange_idp.base_url,
            "--exchange-client-id",
            "exchange-cli",
        ])
        .await?;

    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "exchanged-token",
        "--exchange-issuer must resolve the token endpoint via OIDC discovery and then emit the \
         EXCHANGED token"
    );
    assert_eq!(
        exchange_idp.token_call_count()?,
        1,
        "the token endpoint discovered under --exchange-issuer must actually have been called"
    );
    Ok(())
}

#[tokio::test]
async fn otel_headers_also_emits_the_exchanged_token() -> Result<()> {
    let exchange_idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "exchanged-token".to_owned(),
        refresh_token: None,
        expires_in: 900,
    })
    .await?;

    let harness = Harness::new("https://unreachable.invalid.example")?;
    seed_fresh_session(&harness, "upstream-access-token")?;

    let output = harness
        .run(&[
            "otel-headers",
            "--token-exchange",
            "--exchange-token-endpoint",
            &format!("{}/token", exchange_idp.base_url),
            "--exchange-client-id",
            "exchange-cli",
        ])
        .await?;

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let headers: serde_json::Value = serde_json::from_str(stdout.trim()).context("parse json")?;
    assert_eq!(headers["Authorization"], "Bearer exchanged-token");
    Ok(())
}

/// THE fail-closed regression test: an exchange that's enabled but rejected
/// by the exchange server must exit non-zero with NOTHING on stdout -- never
/// a silent fallback to the un-exchanged upstream token, which would emit a
/// credential the operator deliberately chose not to use.
#[tokio::test]
async fn token_exchange_enabled_and_rejected_fails_closed() -> Result<()> {
    let exchange_idp = MockIdp::start(TokenBehavior::Fail {
        status: 400,
        error: "invalid_grant",
    })
    .await?;

    let harness = Harness::new("https://unreachable.invalid.example")?;
    seed_fresh_session(&harness, "upstream-access-token")?;

    let output = harness
        .run(&[
            "token",
            "--token-exchange",
            "--exchange-token-endpoint",
            &format!("{}/token", exchange_idp.base_url),
            "--exchange-client-id",
            "exchange-cli",
        ])
        .await?;

    assert!(
        !output.status.success(),
        "a rejected exchange must be a non-zero exit"
    );
    assert!(
        output.stdout.is_empty(),
        "a rejected exchange must print NOTHING to stdout -- never the un-exchanged upstream \
         token as a fallback; got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    Ok(())
}

/// A rejected exchange must still name the fixed OAuth2 error code and HTTP
/// status (both actionable), but must NEVER print the exchange server's own
/// free-text `error_description` verbatim -- an authorization server's error
/// response can echo submitted input (e.g. the subject token) back, and this
/// PR routes `token_endpoint` to a SECOND, less-controlled authorization
/// server. Mirrors the discipline `oauth::mod::revoke` already applies to a
/// revocation error body.
#[tokio::test]
async fn token_exchange_rejection_does_not_leak_the_servers_error_description() -> Result<()> {
    let exchange_idp = MockIdp::start(TokenBehavior::Fail {
        status: 400,
        error: "invalid_grant",
    })
    .await?;

    let harness = Harness::new("https://unreachable.invalid.example")?;
    seed_fresh_session(&harness, "upstream-access-token")?;

    let output = harness
        .run(&[
            "token",
            "--token-exchange",
            "--exchange-token-endpoint",
            &format!("{}/token", exchange_idp.base_url),
            "--exchange-client-id",
            "exchange-cli",
        ])
        .await?;

    assert!(
        !output.status.success(),
        "a rejected exchange must be a non-zero exit"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid_grant"),
        "the fixed OAuth2 error code must still be reported so the failure is actionable, got: \
         {stderr}"
    );
    assert!(
        stderr.contains("400"),
        "the HTTP status must still be reported, got: {stderr}"
    );
    assert!(
        !stderr.contains("mock idp configured failure"),
        "the exchange server's free-text `error_description` must never reach stderr, got: \
         {stderr}"
    );
    Ok(())
}

/// Same fail-closed property, but for a network-level failure (exchange
/// endpoint unreachable) rather than an HTTP-level rejection -- a different
/// code path inside `oauth::exchange::run`/`token_endpoint::request`.
#[tokio::test]
async fn token_exchange_enabled_against_an_unreachable_endpoint_fails_closed() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    seed_fresh_session(&harness, "upstream-access-token")?;

    let output = harness
        .run(&[
            "token",
            "--token-exchange",
            "--exchange-token-endpoint",
            "https://exchange.unreachable.invalid.example/token",
            "--exchange-client-id",
            "exchange-cli",
        ])
        .await?;

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    Ok(())
}

/// A THIRD fail-closed code path, distinct from the two above: the exchange
/// endpoint returns a SUCCESS status (200) but a body that isn't JSON at
/// all -- `token_endpoint::request`'s success-path
/// `serde_json::from_str::<RawTokenResponse>` must reject it rather than
/// this binary emitting a fabricated/empty token as if the exchange had
/// actually succeeded.
#[tokio::test]
async fn token_exchange_malformed_200_body_fails_closed() -> Result<()> {
    let exchange_idp = MockIdp::start(TokenBehavior::MalformedSuccess).await?;

    let harness = Harness::new("https://unreachable.invalid.example")?;
    seed_fresh_session(&harness, "upstream-access-token")?;

    let output = harness
        .run(&[
            "token",
            "--token-exchange",
            "--exchange-token-endpoint",
            &format!("{}/token", exchange_idp.base_url),
            "--exchange-client-id",
            "exchange-cli",
        ])
        .await?;

    assert!(
        !output.status.success(),
        "a malformed (non-JSON) 200 body from the exchange endpoint must be a non-zero exit"
    );
    assert!(
        output.stdout.is_empty(),
        "a malformed 200 body must print NOTHING to stdout -- never a fabricated token; got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    Ok(())
}

/// Enabling exchange without an `--exchange-client-id` must fail closed at
/// config-resolution time, before any network call -- not launch anyway
/// with some empty/default client id.
#[tokio::test]
async fn token_exchange_enabled_without_a_client_id_fails_closed() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    seed_fresh_session(&harness, "upstream-access-token")?;

    let output = harness
        .run(&[
            "token",
            "--token-exchange",
            "--exchange-token-endpoint",
            "https://exchange.example/token",
        ])
        .await?;

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--exchange-client-id"),
        "error should name the missing flag, got: {stderr}"
    );
    Ok(())
}
