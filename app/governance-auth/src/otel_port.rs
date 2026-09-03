//! The loopback endpoint `serve --otel` binds and every client posts OTLP to.
//!
//! **Contract, not preference**, like [`crate::oauth::callback_port`]: written
//! into every client's config by `configure` and read once at process start, so
//! changing it later breaks every already-configured client.

use std::net::TcpListener;

use anyhow::{Context, Result};

/// The loopback port `serve --otel` binds, just above the ADR-0015 callback
/// block (17452–17456) and clear of every [`crate::oauth::CALLBACK_PORTS`]
/// value. In the window that matters: **below 32768** (outside both OS
/// ephemeral ranges) and **above 1024** (no root needed). The specific value
/// is arbitrary; the window is load-bearing (ADR-0015).
///
/// No `dead_code` suppression: [`OTEL_LOOPBACK_ENDPOINT`] derives from this
/// at compile time, and that constant is consumed by `oauth::apply_telemetry`
/// (#270 AC1) -- which makes this one transitively used too. The suppression
/// that used to sit here went stale (`unfulfilled_lint_expectations`) the
/// moment that landed; removed rather than left to warn.
pub const OTEL_PORT: u16 = 17457;

/// The URL shape clients are configured to post OTLP to. Derived from
/// [`OTEL_PORT`] so the two cannot drift to different ports. The receiver
/// accepts **any** path on this endpoint: Codex posts to its configured
/// endpoint verbatim, appending no signal path; signal-path normalisation
/// belongs to the receiver in #268 proper.
///
/// Consumed by `oauth::apply_telemetry` (#270 AC1) -- the `dead_code`
/// suppression that used to sit here has been removed rather than left
/// stale, per the reason it originally gave.
pub const OTEL_LOOPBACK_ENDPOINT: &str = const_format::formatcp!("http://127.0.0.1:{}", OTEL_PORT);

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
    use std::{
        io::{Read, Write},
        net::{Shutdown, TcpStream},
        sync::{Mutex, OnceLock},
    };

    use super::*;

    /// Serializes tests that bind the single fixed port, so a loser does not
    /// race the winner; the loser fails loudly rather than silently skipping.
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

    /// Loopback-only (ADR-0016: never 0.0.0.0 or a lookalike domain), naming
    /// the fixed port.
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
        // Hold the port so the bind below must refuse. If we cannot hold it,
        // another process already does — and a fixed port being taken is itself
        // a violation of the contract. Either way the refusal below MUST hold,
        // so fail rather than report green having tested nothing.
        let _held = TcpListener::bind(("127.0.0.1", OTEL_PORT))
            .expect("cannot hold the OTEL port; another process already binds it");

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

    /// Drives a real TCP POST to the documented endpoint; the static checks
    /// above could all stay green while a client could not actually connect.
    #[test]
    fn a_real_post_to_the_endpoint_reaches_the_bound_listener() {
        let _guard = port_lock().lock().unwrap_or_else(|p| p.into_inner());
        let listener = bind_loopback().expect("cannot bind the OTEL port; another process does");

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
