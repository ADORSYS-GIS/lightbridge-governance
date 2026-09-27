//! `--callback-port`/`--callback-bind` (issue #364) through the real
//! compiled binary -- the pairwise file-layer proofs live in the unit tests
//! (`config::tests::callback`); this file proves what those cannot see: the
//! listener really binds where asked and the value really lands on the wire.
//!
//! The five registered ports are `oauth::callback_port::CALLBACK_PORTS`;
//! this binary crate has no `[lib]` target, so an integration test cannot
//! import them and repeats the literals (as `tests/otel_port.rs` does too).

mod support;

use anyhow::{Context, Result};
use support::{
    harness::{Harness, correct_state_action},
    mock_idp::{MockIdp, TokenBehavior},
};

const CALLBACK_PORTS: [u16; 5] = [17452, 17453, 17454, 17455, 17456];

/// The `redirect_uri` query parameter off a captured authorize URL.
fn redirect_uri_of(authorize_url: &str) -> Result<String> {
    url::Url::parse(authorize_url)
        .context("parsing authorize url")?
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .map(|(_, value)| value.into_owned())
        .context("authorize url missing redirect_uri")
}

async fn succeeding_idp() -> Result<MockIdp> {
    MockIdp::start(TokenBehavior::Succeed {
        access_token: "issued-access-token".to_owned(),
        refresh_token: Some("issued-refresh-token".to_owned()),
        expires_in: 300,
    })
    .await
}

fn assert_login_succeeded(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "login failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[tokio::test]
async fn an_explicit_callback_port_is_bound_and_carried_on_the_authorize_url() -> Result<()> {
    let idp = succeeding_idp().await?;

    let harness = Harness::new(&idp.base_url)?;
    let port = CALLBACK_PORTS[2].to_string();

    let (output, authorize_url) = harness
        .login_with_args_capturing_authorize_url(
            &["--callback-port", &port],
            &[],
            correct_state_action,
        )
        .await?;

    assert_login_succeeded(&output);
    let redirect_uri = redirect_uri_of(&authorize_url)?;
    assert!(
        redirect_uri.starts_with(&format!("http://127.0.0.1:{port}/")),
        "redirect_uri must carry the requested port, got: {redirect_uri}"
    );
    Ok(())
}

/// `--callback-bind 0.0.0.0`: the LISTEN address changes, but the authorize
/// URL's `redirect_uri` host does not -- it stays `127.0.0.1`, which is what
/// the authorization server has registered.
#[tokio::test]
async fn callback_bind_changes_the_listener_not_the_redirect_uri_host() -> Result<()> {
    let idp = succeeding_idp().await?;

    let harness = Harness::new(&idp.base_url)?;

    let (output, authorize_url) = harness
        .login_with_args_capturing_authorize_url(
            &["--callback-bind", "0.0.0.0"],
            &[],
            correct_state_action,
        )
        .await?;

    assert_login_succeeded(&output);
    let redirect_uri = redirect_uri_of(&authorize_url)?;
    assert!(
        redirect_uri.starts_with("http://127.0.0.1:"),
        "redirect_uri host must stay 127.0.0.1 regardless of --callback-bind, got: {redirect_uri}"
    );
    Ok(())
}

/// Unset entirely, `login` must behave exactly as before these flags
/// existed: bind the first free port of the block, on loopback.
#[tokio::test]
async fn with_neither_flag_set_the_previous_behaviour_is_unchanged() -> Result<()> {
    let idp = succeeding_idp().await?;

    let harness = Harness::new(&idp.base_url)?;

    let (output, authorize_url) = harness
        .login_with_args_capturing_authorize_url(&[], &[], correct_state_action)
        .await?;

    assert_login_succeeded(&output);
    let redirect_uri = redirect_uri_of(&authorize_url)?;
    let bound_port = url::Url::parse(&redirect_uri)
        .context("parsing redirect_uri")?
        .port()
        .context("redirect_uri has no port")?;
    assert!(
        CALLBACK_PORTS.contains(&bound_port),
        "bound {bound_port}, which is outside the registered block"
    );
    assert!(redirect_uri.starts_with("http://127.0.0.1:"));
    Ok(())
}

/// Layer 1 vs layer 2 (clap flag vs its own env var), through the real
/// process environment and the real loopback listener -- the same proof
/// `tests/config_precedence.rs` makes for `--scopes`, for `--callback-port`.
#[tokio::test]
async fn callback_port_flag_wins_over_its_env_var_through_real_clap_parsing() -> Result<()> {
    let idp = succeeding_idp().await?;

    let harness = Harness::new(&idp.base_url)?;
    let flag_port = CALLBACK_PORTS[3].to_string();
    let env_port = CALLBACK_PORTS[4].to_string();

    let (output, authorize_url) = harness
        .login_with_args_capturing_authorize_url(
            &["--callback-port", &flag_port],
            &[("GOVERNANCE_AUTH_CALLBACK_PORT", &env_port)],
            correct_state_action,
        )
        .await?;

    assert_login_succeeded(&output);
    let redirect_uri = redirect_uri_of(&authorize_url)?;
    assert!(
        redirect_uri.starts_with(&format!("http://127.0.0.1:{flag_port}/")),
        "an explicit --callback-port must win over GOVERNANCE_AUTH_CALLBACK_PORT, but the \
         redirect_uri carried a different port: {redirect_uri}"
    );
    Ok(())
}
