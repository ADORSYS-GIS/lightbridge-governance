//! `serve --otel`: the local collector daemon (ADR-0016, issue #268).
//!
//! Receives OTLP/HTTP on the fixed loopback port, mints a fresh bearer per
//! forward through the same `oauth` path `token` uses, and posts to the
//! governed collector. No client is ever handed a credential — the bearer
//! exists only inside this process, on its own outbound request.
//!
//! ## The one property that matters: fail closed
//!
//! **The unavailable branch is the restrictive branch.** A refused mint or an
//! unreachable collector means *withhold*, never *allow* — `unwrap_or(false)`
//! on a check is how an outage becomes an authorization bypass. The client
//! gets a low-latency "accepted" the moment bytes are spooled, not the
//! moment the collector answers; an outage costs latency, never data.
//!
//! ## And the second: nothing is lost quietly
//!
//! Every admitted payload goes to the durable spool before the daemon answers
//! its sender. One drain path then handles authentication, forwarding, retry,
//! and confirmed permanent refusal. That ordering matters: whether a payload
//! receives quarantine protection must be a property of the payload and the
//! collector, not of whether the collector happened to be reachable when the
//! payload first arrived. A failed `retain` answers `503`, never success the
//! payload never earned. [`spool::DurableSpool`] writes to disk,
//! `fsync`-durably, before this handler ever answers the client (#269), so a
//! killed daemon -- or laptop -- loses at most the narrow exception that
//! module's doc names.

mod checkpoint;
mod classify;
mod codex_cost;
mod codex_sessions;
mod drain;
mod forward;
mod log_rotation;
mod mint;
mod normalize;
mod protobuf;
mod receive;
mod shutdown;
mod signal;
mod spool;
mod spool_compaction;
mod status;

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
pub use status::DaemonSpoolStatus;
use tokio::{net::TcpListener, sync::Notify};

use crate::{config::OauthConfig, otel_port};

/// Shared state for every request the daemon handles.
///
/// `config` is `Arc`-wrapped: axum's `State<S>` clones `S` per request, and
/// cloning the whole `OauthConfig` was a fresh allocation per field on every
/// request; `Arc::clone` is a refcount bump.
#[derive(Clone)]
struct DaemonState {
    http: reqwest::Client,
    config: Arc<OauthConfig>,
    spool: Arc<Mutex<spool::DurableSpool>>,
    drain_wake: Arc<Notify>,
}

/// Runs the daemon until a shutdown signal arrives.
///
/// Binds the fixed loopback port ([`otel_port::bind_loopback`]), which
/// refuses to fall back to an ephemeral port — a fallback would leave the
/// receiver where no client's telemetry can arrive. Runs on the accept loop
/// until SIGTERM/SIGINT; the spool itself needs no shutdown step, being
/// durable on disk rather than in memory (#269).
pub async fn serve(http: &reqwest::Client, config: &OauthConfig) -> Result<()> {
    config.otel_endpoint.as_deref().context(
        "no collector configured: supply --otel-endpoint / GOVERNANCE_AUTH_OTEL_ENDPOINT (or set \
         `otel_endpoint` in your config file) before running `serve --otel`",
    )?;

    let listener = otel_port::bind_loopback()?;
    // `bind_loopback` hands back a blocking std listener (its unit tests
    // accept on it with std threads); tokio needs a nonblocking socket
    // before `from_std`, which panics on a blocking one -- flip the flag
    // here, the one and only adoption site.
    listener
        .set_nonblocking(true)
        .context("setting the OTEL loopback listener nonblocking for tokio")?;
    let listener = TcpListener::from_std(listener)
        .context("adopting the bound OTEL loopback listener into tokio")?;

    let state = DaemonState {
        http: http.clone(),
        config: Arc::new(config.clone()),
        spool: Arc::new(Mutex::new(
            spool::DurableSpool::open().context("opening the daemon's durable spool")?,
        )),
        drain_wake: Arc::new(Notify::new()),
    };

    // Keeps retrying independent of client traffic -- see `drain::pump`'s
    // doc. Aborted below once the server itself stops; a detached task
    // would otherwise outlive the listener with nothing to hand results to.
    let pump = tokio::spawn(drain::pump(state.clone()));
    // Re-checks the log file's size on its own schedule -- `logging::init`'s
    // startup check only bounds a short-lived process, and this daemon is
    // the opposite of one. See `log_rotation`'s doc for why it cannot just
    // ride along on `pump` instead. Aborted alongside it for the same
    // reason: nothing should outlive the listener it exists to serve.
    let rotation = tokio::spawn(log_rotation::ticker());
    // Closes the gap try_reclaim's own truncate cannot: see
    // `spool_compaction`'s doc. Same independent-timer reasoning as
    // `rotation` above, and aborted alongside the other two for the same
    // reason.
    let compaction = tokio::spawn(spool_compaction::ticker(state.clone()));
    let codex_sessions = tokio::spawn(codex_sessions::ticker(state.clone()));

    let router = Router::new()
        .fallback(any(handle_request))
        .with_state(state);

    let result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown::signal())
        .await
        .context("running the OTEL loopback receiver");
    pump.abort();
    let _ = pump.await;
    rotation.abort();
    let _ = rotation.await;
    compaction.abort();
    let _ = compaction.await;
    codex_sessions.abort();
    let _ = codex_sessions.await;
    result
}

/// Handles one OTLP request: receive -> classify -> durable admission.
///
/// Forwarding belongs exclusively to the background drain. Keeping the
/// network out of this handler makes the acknowledgement precise: `200`
/// means this daemon has durably accepted custody, independent of the online
/// collector's latency or current verdict. OTLP defines `200`, rather than
/// HTTP's asynchronous `202`, as its full-success response.
async fn handle_request(
    State(state): State<DaemonState>,
    request: axum::extract::Request,
) -> Response {
    // Admission FIRST: `receive::build`'s `Host`/`Content-Type` checks make
    // an untrusted request free because no disk or credentialed work runs
    // before them.
    let incoming = match receive::build(request).await {
        Ok(incoming) => incoming,
        Err(receive::ReceiveError::UntrustedHost) => {
            tracing::warn!("refusing a request with an untrusted Host header");
            return StatusCode::FORBIDDEN.into_response();
        }
        Err(receive::ReceiveError::UnsupportedContentType) => {
            return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
        }
        Err(receive::ReceiveError::Body(error)) => {
            tracing::warn!(error = %error, "could not read the request body");
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        }
    };

    // Path is diagnostic metadata and the explicit OTLP signal discriminator.
    tracing::trace!(method = %incoming.method, path = %incoming.path, "received OTLP");
    // Classification is the only inspection needed at admission. Identity
    // stamping happens when the drain forwards the retained bytes.
    let Some(signal) = classify::signal(&incoming.body, incoming.format, &incoming.path) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let body = if signal == signal::Signal::Logs {
        codex_cost::enrich(&incoming.body, incoming.format)
    } else {
        incoming.body
    };
    retained_response(&state, signal, body, incoming.format).await
}

/// Retains `payload` and answers what actually happened: an OTLP full-success
/// response when it is durably queued, `503` when the spool could not retain
/// it. The success body is the empty ExportLogsServiceResponse /
/// ExportMetricsServiceResponse encoding: `{}` for JSON, zero bytes for
/// protobuf, with the same content type the sender used as OTLP requires.
async fn retained_response(
    state: &DaemonState,
    signal: signal::Signal,
    payload: Vec<u8>,
    format: receive::WireFormat,
) -> Response {
    if drain::retain(state, signal, payload, format).await {
        let content_type = [(header::CONTENT_TYPE, format.content_type())];
        match format {
            receive::WireFormat::Json => (StatusCode::OK, content_type, "{}").into_response(),
            receive::WireFormat::Protobuf => {
                (StatusCode::OK, content_type, Vec::<u8>::new()).into_response()
            }
        }
    } else {
        // Spool capacity is backpressure, not a permanent payload verdict.
        // Give an exporter a concrete floor for retry instead of inviting a
        // tight loop while the drain is already stalled.
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "5")],
        )
            .into_response()
    }
}
