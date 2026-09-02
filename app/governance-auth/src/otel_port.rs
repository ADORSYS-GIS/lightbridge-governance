//! The loopback endpoint `serve --otel` binds and every client posts OTLP to.
//!
//! **Contract, not preference**, like [`crate::oauth::callback_port`], and must
//! not collide with it. Written into every client's config by `configure` and
//! read once at process start, so changing it later breaks every
//! already-configured client.

use std::net::TcpListener;

use anyhow::{Context, Result};

/// The loopback port `serve --otel` binds and clients are configured to post
/// to, chosen just above the ADR-0015 callback block (17452–17456) and clear
/// of every [`crate::oauth::CALLBACK_PORTS`] value (all five are the login
/// flow's listeners). Why this window: **below 32768** — outside both OS
/// ephemeral ranges (Linux 32768+, macOS/IANA 49152+) so the OS cannot hand it
/// to an unrelated process; **above 1024** — binding needs no root. Past those,
/// the specific value is arbitrary; the *window* is load-bearing (ADR-0015).
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "shipped contract for #268's `serve --otel` daemon and #270/#271; remove once a non-test consumer exists"
    )
)]
pub const OTEL_PORT: u16 = 17457;

/// The URL shape clients are configured to post OTLP to.
///
/// The receiver accepts **any** path on this endpoint: Codex posts OTLP to its
/// configured endpoint verbatim, appending no signal path. Signal-path
/// normalisation belongs to the receiver in #268 proper; this only pins the
/// port and the loopback-only shape.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "shipped contract for #268's `serve --otel` daemon and #270/#271; remove once a non-test consumer exists"
    )
)]
pub const OTEL_LOOPBACK_ENDPOINT: &str = "http://127.0.0.1:17457";

/// Binds the fixed loopback OTEL port, refusing to fall back to an ephemeral
/// one — a fallback would leave the receiver where no client's telemetry can
/// arrive, and every client's bytes would vanish silently, so it fails loudly.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "receiver bootstrapping for #268's `serve --otel` daemon; remove once the daemon lands"
    )
)]
pub fn bind_loopback() -> Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", OTEL_PORT))
        .with_context(|| format!("binding the OTEL loopback receiver on 127.0.0.1:{OTEL_PORT}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::{Shutdown, TcpStream},
        sync::{Mutex, OnceLock},
    };

    /// Serializes tests that bind the single fixed port (Rust runs tests in
    /// parallel; with one port a loser's bind fails and reports "skipped").
    fn port_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// Outside both ephemeral ranges is what makes this port safe to pin.
    /// Compile-time (`const` block), so it fails the build when it stops
    /// holding instead of only failing a test that could rot silently.
    #[test]
    fn is_outside_both_ephemeral_ranges() {
        const _: () = const {
            assert!(OTEL_PORT > 1024, "OTEL_PORT needs root to bind");
            assert!(
                OTEL_PORT < 32768,
                "OTEL_PORT is inside an OS ephemeral range and can be taken by another process"
            );
        };
    }

    /// The whole guard against colliding with the OAuth redirect listeners.
    #[test]
    fn is_distinct_from_callback_ports() {
        assert!(
            !crate::oauth::CALLBACK_PORTS.contains(&OTEL_PORT),
            "the OTEL port {OTEL_PORT} collides with a loopback callback port"
        );
    }

    /// Loopback-only (ADR-0016: never 0.0.0.0 or a lookalike domain), naming the
    /// fixed port.
    #[test]
    fn endpoint_is_loopback_only_and_names_the_fixed_port() {
        let parsed = url::Url::parse(OTEL_LOOPBACK_ENDPOINT).expect("endpoint parses as a URL");
        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
        assert_eq!(parsed.port(), Some(OTEL_PORT));
    }

    /// Binding must NOT silently fall back to an ephemeral port.
    #[test]
    fn bind_refuses_rather_than_falling_back_to_an_ephemeral_port() {
        let _guard = port_lock().lock().unwrap_or_else(|p| p.into_inner());
        // Hold the port so the bind below must refuse; skip (never abort) if held.
        let Ok(_held) = TcpListener::bind(("127.0.0.1", OTEL_PORT)) else {
            eprintln!("skipped: port {OTEL_PORT} already unavailable");
            return;
        };

        let error = bind_loopback().expect_err("must refuse when the fixed port is taken");
        let message = format!("{error:#}");
        assert!(
            message.contains(&OTEL_PORT.to_string()),
            "names the port: {message}"
        );
        assert!(
            message.contains("127.0.0.1"),
            "names the address: {message}"
        );
    }

    /// Drives a real TCP POST to the documented endpoint; the static checks above
    /// could all stay green while a client could not actually connect.
    #[test]
    fn a_real_post_to_the_endpoint_reaches_the_bound_listener() {
        let _guard = port_lock().lock().unwrap_or_else(|p| p.into_inner());
        let listener = match bind_loopback() {
            Ok(l) => l,
            Err(_) => {
                eprintln!("skipped: port {OTEL_PORT} already unavailable");
                return;
            }
        };

        let server = std::thread::spawn(move || {
            let (mut stream, _peer) = listener.accept().expect("client must connect");
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .expect("set read timeout");
            let mut request = Vec::new();
            let mut chunk = [0u8; 512];
            while let Ok(n) = stream.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..n]);
            }
            let request = String::from_utf8_lossy(&request);
            assert!(
                request.starts_with("POST /"),
                "must have POSTed to the endpoint, got:\n{request}"
            );
            assert!(
                request.contains("resourceLogs"),
                "the OTLP body must have arrived, got:\n{request}"
            );
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .expect("reply 200");
        });

        let url = url::Url::parse(OTEL_LOOPBACK_ENDPOINT).expect("endpoint parses as a URL");
        let host = url.host_str().expect("endpoint has a host");
        let port = url.port().expect("endpoint has a port");

        let mut stream =
            TcpStream::connect((host, port)).expect("connect to the documented endpoint");
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("set write timeout");
        let body = r#"{"resourceLogs":[{}]}"#;
        let request = format!(
            "POST / HTTP/1.1\r\nhost: {host}:{port}\r\ncontent-type: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).expect("send the POST");
        stream.shutdown(Shutdown::Write).expect("half-close");

        let mut response = Vec::new();
        let mut chunk = [0u8; 256];
        while let Ok(n) = stream.read(&mut chunk) {
            if n == 0 {
                break;
            }
            response.extend_from_slice(&chunk[..n]);
        }
        let response = String::from_utf8_lossy(&response);
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "listener must answer 200: {response}"
        );
        server.join().expect("server thread finished cleanly");
    }
}
