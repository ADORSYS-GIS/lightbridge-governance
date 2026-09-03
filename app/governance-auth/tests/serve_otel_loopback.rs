//! `serve --otel` (issue #268): loopback-only binding (A1). The daemon binds
//! `127.0.0.1`, never a wildcard, so a non-loopback address of this host must
//! refuse the connection.
//!
//! The non-loopback probe is environment-dependent (a host with no network
//! interface has none to probe), so when no such address exists the LAN half is
//! skipped **loudly** — never a silent green — leaving the loopback half and the
//! unit-level bind assertions in `src/otel_port.rs` to carry the property.

mod support;

use anyhow::Result;
use support::{
    copilot as fixture,
    harness::Harness,
    mock_collector::{Behavior, MockCollector},
    otel_payload::logs_payload,
    serve_otel::Daemon,
};

#[tokio::test]
async fn loopback_only_refuses_a_non_loopback_address() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let collector = MockCollector::start(Behavior::Accept).await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;

    // Loopback half: the documented endpoint must be reachable.
    let status = daemon.post("/", &logs_payload("loopback")).await?;
    assert_eq!(status.as_u16(), 200);

    // LAN half: find a non-loopback address on this host.
    if let Some(lan) = non_loopback_address() {
        let refused = std::net::TcpStream::connect((lan, 17457)).is_err();
        assert!(
            refused,
            "the daemon must not listen on the wildcard: {lan}:17457 was reachable"
        );
    } else {
        eprintln!(
            "skip: host has no non-loopback interface to probe loopback-only binding \
             (covered at the unit level by src/otel_port.rs)"
        );
    }

    daemon.stop()?;
    Ok(())
}

/// A best-effort probe for a non-loopback address of this host. Uses a UDP
/// socket "connected" to a public resolver so the kernel picks the interface
/// that would carry real traffic, without sending a packet. Returns `None` on
/// a host that has no non-loopback interface.
fn non_loopback_address() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect(("8.8.8.8", 80)).ok()?;
    let local = socket.local_addr().ok()?;
    let ip = local.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        None
    } else {
        Some(ip)
    }
}
