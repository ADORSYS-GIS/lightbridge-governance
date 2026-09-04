//! `serve --otel` (issue #268): loopback-only binding (A1). The daemon binds
//! `127.0.0.1`, never a wildcard, so a non-loopback address of this host must
//! refuse the connection.
//!
//! ## Why a plain `connect().is_err()` does not prove this (#290 review)
//!
//! A blocking `connect` with no timeout cannot distinguish "nothing is
//! listening" (an active RST — proof the property holds) from "a firewall
//! rule silently drops the packet" (a hang, then an OS-default timeout that
//! can be minutes away). The second case is `Err` too, so the naive assertion
//! passes on a firewalled host **whether or not the daemon actually bound the
//! wildcard** — exactly the bug this test exists to catch, made invisible.
//! [`probe`] uses a short, explicit `connect_timeout` and reports which of
//! the three real outcomes happened, so [`loopback_only_refuses_a_non_loopback_address`]
//! can fail loudly on the one that proves nothing rather than read it as a
//! pass.
//!
//! The non-loopback probe is also environment-dependent (a host with no
//! non-loopback interface has none to probe). Per AGENTS.md ("assert the
//! tests actually ran"), that is a hard failure here, not a silent skip: a
//! green run must mean the property was checked, and an operator seeing this
//! fail on a `lo`-only host has an actionable, honest reason why, rather than
//! a green suite that quietly checked nothing.

mod support;

use std::{
    net::{SocketAddr, TcpStream},
    time::Duration,
};

use anyhow::Result;
use support::{
    copilot as fixture,
    harness::Harness,
    mock_collector::{Behavior, MockCollector},
    otel_payload::logs_payload,
    serve_otel::Daemon,
};

/// Long enough that a real RST or a real accept both land well inside it on
/// any host under any load; short enough that a silently-dropped packet does
/// not make this test hang for the OS-default timeout (which can be minutes).
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[tokio::test]
async fn loopback_only_refuses_a_non_loopback_address() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let collector = MockCollector::start(Behavior::Accept).await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;

    // Loopback half: the documented endpoint must be reachable.
    let status = daemon.post("/", &logs_payload("loopback")).await?;
    assert_eq!(status.as_u16(), 200);

    // LAN half: find a non-loopback address on this host, and probe it.
    let lan = non_loopback_address().expect(
        "this host has no non-loopback interface to probe loopback-only binding against; the \
         property is unchecked, not passing -- run this test on a host that has one, or add a \
         platform-specific probe",
    );
    match probe(SocketAddr::from((lan, 17457))) {
        Probe::Refused => {}
        Probe::Connected => {
            panic!("the daemon must not listen on the wildcard: {lan}:17457 accepted a connection")
        }
        Probe::TimedOut => panic!(
            "connecting to {lan}:17457 neither succeeded nor was refused within {PROBE_TIMEOUT:?} \
             -- inconclusive (a firewall rule silently dropping the packet looks identical to a \
             refusal here), so this proves nothing either way. Run outside whatever is filtering \
             this host's own loopback-adjacent traffic."
        ),
    }

    daemon.stop()?;
    Ok(())
}

enum Probe {
    /// The connection was actively refused (or reset) -- proof nothing
    /// listens at this address.
    Refused,
    /// The connection succeeded -- proof the daemon (or something) does.
    Connected,
    /// Neither happened within [`PROBE_TIMEOUT`] -- inconclusive.
    TimedOut,
}

/// Distinguishes an active refusal from a silent drop, which a bare
/// `connect().is_err()` cannot -- see the module doc.
fn probe(address: SocketAddr) -> Probe {
    match TcpStream::connect_timeout(&address, PROBE_TIMEOUT) {
        Ok(_) => Probe::Connected,
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => Probe::TimedOut,
        Err(_) => Probe::Refused,
    }
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
