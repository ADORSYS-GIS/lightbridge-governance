//! ADR-0012 Decision 2's layer 1 vs layer 2: an explicit `--scopes` flag
//! must win over `GOVERNANCE_AUTH_SCOPES`.
//!
//! This is the one precedence step that happens *inside* clap, before
//! `config::OauthConfigArgs` is ever constructed by hand -- see that
//! module's `config::tests::precedence` submodule for the other three
//! pairwise proofs (env-or-flag vs per-user file, per-user vs machine-wide,
//! machine-wide vs compiled default), which drive `resolve_with_paths`
//! directly and don't need a real process or a real IdP.
//!
//! Proved end-to-end against the real value the client puts on the wire
//! (the device-authorization request's `scope` form field), through a real
//! subprocess with `GOVERNANCE_AUTH_SCOPES` set in *that child's*
//! environment only -- `Command::env`, not `std::env::set_var`, which this
//! workspace's `unsafe_code = "deny"` forbids outright (Rust 2024 made
//! `set_var` `unsafe`).

mod support;

use anyhow::{Context, Result};
use support::{
    harness::Harness,
    mock_idp::{MockIdp, TokenBehavior},
};

#[tokio::test]
async fn flag_beats_env_for_scopes() -> Result<()> {
    let idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "issued-access-token".to_owned(),
        refresh_token: Some("issued-refresh-token".to_owned()),
        expires_in: 300,
    })
    .await?;

    let harness = Harness::new(&idp.base_url)?;
    let output = harness
        .run_with_env(
            &["login", "--device-code", "--scopes", "flag-scope"],
            &[("GOVERNANCE_AUTH_SCOPES", "env-scope")],
        )
        .await?;

    assert!(
        output.status.success(),
        "device-code login failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let scope_sent = idp
        .last_device_scope()?
        .context("device-authorization request carried no scope at all")?;
    assert_eq!(
        scope_sent, "flag-scope",
        "an explicit --scopes flag must win over GOVERNANCE_AUTH_SCOPES, but the request on \
         the wire carried the env var's value"
    );
    Ok(())
}
