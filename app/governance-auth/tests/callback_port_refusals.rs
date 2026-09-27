//! `--callback-port` refusal paths (issue #364): a port outside the
//! registered block, and a chosen port that is already busy. Split out of
//! `callback_port_option.rs` purely to keep that file under the loc-gate's
//! 200-line threshold for a new file.
//!
//! The out-of-block port is refused before `login` ever calls OIDC
//! discovery, so that test needs no working IdP at all. The busy-port
//! refusal happens later, inside `oauth::authcode::run` -- discovery must
//! succeed first for `login` to reach it -- so that test needs the same
//! `MockIdp` its sibling tests in `callback_port_option.rs` use.

mod support;

use std::time::Instant;

use anyhow::Result;
use support::{
    harness::Harness,
    mock_idp::{MockIdp, TokenBehavior},
};

const CALLBACK_PORTS: [u16; 5] = [17452, 17453, 17454, 17455, 17456];

/// Validated (and refused) purely by parsing a `u16` and checking block
/// membership -- no `TcpListener`, no HTTP client -- so this must fail
/// before `login` ever calls OIDC discovery against `--issuer`. Proved here
/// by pointing `--issuer` at a domain that cannot resolve (`.invalid`, RFC
/// 2606) and yet still getting the PORT error back, fast, rather than a
/// discovery/network failure.
#[tokio::test]
async fn a_port_outside_the_registered_block_is_refused_before_discovery() -> Result<()> {
    let harness = Harness::new("https://issuer.invalid/realms/test")?;
    let started = Instant::now();

    let output = harness.run(&["login", "--callback-port", "9999"]).await?;

    assert!(
        !output.status.success(),
        "a port outside CALLBACK_PORTS must be refused"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("9999"), "should name the port: {stderr}");
    for port in CALLBACK_PORTS {
        assert!(
            stderr.contains(&port.to_string()),
            "should name the registered block; {port} missing from: {stderr}"
        );
    }
    assert!(
        started.elapsed().as_secs() < 5,
        "a purely local validation must fail near-instantly, not after a network attempt \
         against an unresolvable issuer -- took {:?}",
        started.elapsed()
    );
    Ok(())
}

/// A busy chosen port is refused BY NAME, never silently retried on a
/// sibling port from the block.
#[tokio::test]
async fn a_busy_explicit_callback_port_is_refused_without_falling_back() -> Result<()> {
    let port = CALLBACK_PORTS[0];
    let Ok(held) = std::net::TcpListener::bind(("127.0.0.1", port)) else {
        eprintln!("skipped: port {port} already unavailable");
        return Ok(());
    };

    let idp = MockIdp::start(TokenBehavior::Succeed {
        access_token: "should-never-be-issued".to_owned(),
        refresh_token: None,
        expires_in: 300,
    })
    .await?;
    let harness = Harness::new(&idp.base_url)?;
    let output = harness
        .run(&["login", "--callback-port", &port.to_string()])
        .await?;

    assert!(!output.status.success(), "a busy chosen port must refuse");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(&port.to_string()), "got: {stderr}");
    for other in CALLBACK_PORTS {
        assert!(
            other == port || !stderr.contains(&other.to_string()),
            "must not mention a different port -- there was no fallback attempt: {stderr}"
        );
    }
    drop(held);
    Ok(())
}
