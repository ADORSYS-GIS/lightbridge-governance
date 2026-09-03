//! `serve otel` (issue #268): the loopback collector daemon, end to end.
//!
//! These tests drive the real binary as a subprocess (see [`support::serve_otel`])
//! against a mock collector and assert the two properties the daemon exists to
//! guarantee:
//!
//! - **fail closed** — an absent session or an unreachable/refusing collector
//!   never forwards anything unauthenticated, and the client still gets a cheap
//!   `202 Accepted` rather than a `500`; and
//! - **nothing is lost quietly** — a payload refused once is retained and
//!   re-forwarded from the in-memory spool the moment the collector is healthy.
//!
//! Every test shares the daemon's one fixed loopback port, so they are
//! serialized by the port lock held inside [`support::serve_otel::Daemon`].

mod support;

use std::sync::Arc;

use anyhow::{Context, Result};
use support::{
    copilot as fixture,
    harness::Harness,
    interrupt,
    mock_collector::{Behavior, MockCollector},
    serve_otel::{Daemon, jwt_session, logs_payload, metrics_payload},
};

/// No session on disk means the daemon cannot mint a bearer, so it must
/// **withhold**: the client gets a cheap `202`, the collector sees zero
/// requests, and nothing is forwarded unauthenticated. This is the whole
/// fail-closed contract.
#[tokio::test]
async fn no_session_returns_accepted_and_forwards_nothing() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    let collector = MockCollector::start(Behavior::Accept).await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;
    let status = daemon.post("/", &logs_payload("no-session")).await?;

    assert_eq!(
        status.as_u16(),
        202,
        "an unauthenticated payload must still get a cheap accepted, not an error"
    );
    assert_eq!(
        collector.request_count()?,
        0,
        "nothing may be forwarded without a bearer"
    );
    daemon.stop()?;
    Ok(())
}

/// An expired, unrefreshable session is the same fail-closed case one step
/// further in: the mint fails *after* config resolution, and the payload is
/// withheld. (Seeding an expired session with no refresh token means
/// `current_session` cannot recover by calling out to the IdP, so the
/// unreachable issuer never matters.)
#[tokio::test]
async fn an_expired_unrefreshable_session_withholds_and_forwards_nothing() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::expired_session(harness.issuer())?)?;
    let collector = MockCollector::start(Behavior::Accept).await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;
    let status = daemon.post("/", &logs_payload("expired")).await?;

    assert_eq!(status.as_u16(), 202);
    assert_eq!(collector.request_count()?, 0);
    daemon.stop()?;
    Ok(())
}

/// A collector that refuses the export (a retryable 500) must not fail the
/// client: it costs a `202`, never a `500`, and the bytes are retained, not
/// dropped. Then once the collector recovers, the very next accepted request
/// piggybacks a drain of the retained payload before handling its own — so the
/// refused bytes are not lost, just delayed.
#[tokio::test]
async fn a_refusing_collector_accepts_the_client_and_retains_until_it_recovers() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let collector = MockCollector::start(Behavior::Reject(500)).await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;

    // First POST: the collector is refusing, so the forward fails, the payload
    // is retained, and the client still walks away with 202.
    let first = daemon.post("/", &logs_payload("marker-first")).await?;
    assert_eq!(
        first.as_u16(),
        202,
        "a refused forward must still be a 202, not a 500"
    );

    // Now the collector is healthy again. The second POST drains the retained
    // first payload, then forwards its own.
    collector.set_behavior(Behavior::Accept)?;
    let second = daemon.post("/", &logs_payload("marker-second")).await?;
    assert_eq!(
        second.as_u16(),
        200,
        "a healthy collector must yield a 200 for the new payload"
    );

    // The piggyback drain must have re-forwarded the retained payload, so the
    // collector should now have taken both, in order.
    interrupt::until(
        "both the retained and the new payload to reach the collector",
        || {
            let bodies = collector.accepted_log_bodies()?;
            Ok(bodies.iter().any(|b| b == "marker-first")
                && bodies.iter().any(|b| b == "marker-second"))
        },
    )
    .await?;

    daemon.stop()?;
    Ok(())
}

/// The daemon accepts **any** path on its loopback endpoint (A2) — Codex posts
/// to its configured endpoint verbatim — and routes the body by signal. A POST
/// to `/garbage` with a `resourceLogs` body must still be classified as logs
/// and land on `/v1/logs`, never rejected for the unknown path.
#[tokio::test]
async fn any_path_is_forwarded_by_body_signal_not_rejected() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let collector = MockCollector::start(Behavior::Accept).await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;
    let status = daemon
        .post("/utter-garbage", &logs_payload("any-path"))
        .await?;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(
        collector.paths()?,
        vec!["/v1/logs".to_owned()],
        "the path must be carried, not branch-decisioned; the body routes the signal"
    );
    daemon.stop()?;
    Ok(())
}

/// The full happy path: a metrics body routes to `/v1/metrics`, a logs body to
/// `/v1/logs`, and both are stamped with the caller's identity (`user.id`) from
/// the minted token's JWT claims — the same attribution `copilot push` uses.
#[tokio::test]
async fn metrics_and_logs_are_routed_and_stamped_with_identity() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&jwt_session(harness.issuer())?)?;
    let collector = MockCollector::start(Behavior::Accept).await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;

    assert_eq!(daemon.post("/", &metrics_payload()).await?.as_u16(), 200);
    assert_eq!(
        daemon.post("/", &logs_payload("identity")).await?.as_u16(),
        200
    );

    let payloads = collector.payloads()?;
    assert_eq!(payloads.len(), 2, "one metrics, one logs forward");
    let paths: Vec<&str> = payloads.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(paths, vec!["/v1/metrics", "/v1/logs"]);

    for (_path, payload) in payloads {
        let stamped = payload
            .pointer("/resourceMetrics/0/resource/attributes")
            .or_else(|| payload.pointer("/resourceLogs/0/resource/attributes"))
            .and_then(|a| a.as_array())
            .context("the forwarded payload must carry stamped identity attributes")?;
        let user_id = stamped
            .iter()
            .find(|a| a.pointer("/key").and_then(|k| k.as_str()) == Some("user.id"))
            .and_then(|a| a.pointer("/value/stringValue").and_then(|v| v.as_str()))
            .context("the forwarded payload must carry a user.id attribute")?;
        assert_eq!(
            user_id, "user-uuid-1234",
            "attribution must match the minted token's `sub` claim"
        );
    }

    daemon.stop()?;
    Ok(())
}

/// The daemon is loopback-only (ADR-0016): it binds `127.0.0.1`, never a
/// wildcard. Connection to the documented loopback endpoint succeeds, and a
/// connection from a non-loopback address of this host is refused.
///
/// The non-loopback probe is environment-dependent (a host with no network
/// interface has none to probe), so when no such address exists the LAN half is
/// skipped **loudly** — never a silent green — leaving the loopback half and the
/// unit-level bind assertions in `src/otel_port.rs` to carry the property.
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

/// A non-JSON (OTLP protobuf) payload must be **forwarded**, not withheld — a
/// real client's default wire format on this daemon. This is the regression
/// tripwire for the JSON-only bug (F1 in the test plan): it accepts raw bytes
/// and asserts the collector got exactly them, routed to the path the URL named,
/// with the protobuf content-type preserved.
#[tokio::test]
async fn a_non_json_body_is_forwarded_verbatim_not_withheld() -> Result<()> {
    let harness = Harness::new("https://unreachable.invalid.example")?;
    harness.seed_session(&fixture::fresh_session(harness.issuer())?)?;
    let collector = RawCollector::start().await?;

    let daemon = Daemon::start(&harness, &collector.base_url, &[]).await?;

    // A protobuf metrics body at the metrics path.
    let metrics_bytes = b"\x0a\x03log\x12\x04test\x00\x01\x02\x03".to_vec();
    let status = daemon
        .post_bytes(
            "/v1/metrics",
            "application/x-protobuf",
            metrics_bytes.clone(),
        )
        .await?;
    assert_eq!(
        status.as_u16(),
        200,
        "a protobuf body with a valid session must be forwarded (200), not withheld (202)"
    );

    let requests = collector.requests()?;
    assert_eq!(requests.len(), 1, "one protobuf forward expected");
    let (path, content_type, body) = &requests[0];
    assert_eq!(
        path, "/v1/metrics",
        "routed by the URL path for a non-JSON body"
    );
    assert_eq!(
        content_type, "application/x-protobuf",
        "wire format preserved"
    );
    assert_eq!(
        body, &metrics_bytes,
        "forwarded verbatim, not re-serialized"
    );

    daemon.stop()?;
    Ok(())
}

/// A minimal raw-bytes OTLP collector: accepts any body and records
/// `(path, content-type, bytes)`. Exists because the shared
/// [`support::mock_collector`] only accepts JSON, and the protobuf case needs a
/// destination that can prove the *bytes* arrived.
type RawRequest = (String, String, Vec<u8>);
type RawState = std::sync::Mutex<Vec<RawRequest>>;

struct RawCollector {
    base_url: String,
    state: Arc<RawState>,
}

impl RawCollector {
    async fn start() -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("binding the raw collector listener")?;
        let addr = listener
            .local_addr()
            .context("reading the raw collector address")?;
        let state: Arc<RawState> = Arc::default();
        let this = Self {
            base_url: format!("http://{addr}"),
            state: state.clone(),
        };

        let router = axum::Router::new().fallback(raw_receive).with_state(state);
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        Ok(this)
    }

    fn requests(&self) -> Result<Vec<RawRequest>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("raw collector mutex poisoned"))?
            .clone())
    }
}

async fn raw_receive(
    axum::extract::State(state): axum::extract::State<Arc<RawState>>,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::http::StatusCode {
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    if let Ok(mut inner) = state.lock() {
        inner.push((uri.path().to_owned(), content_type, body.to_vec()));
    }
    axum::http::StatusCode::OK
}
