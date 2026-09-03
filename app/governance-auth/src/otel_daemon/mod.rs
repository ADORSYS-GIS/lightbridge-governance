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
//! unreachable collector means *withhold*, never *allow* — that is
//! `unwrap_or(false)` on a check is how an outage becomes an authorization
//! bypass. The client gets a low-latency "accepted" the moment bytes are
//! spooled, not the moment the collector answers; a refused mint or an
//! unreachable collector costs latency, never data.
//!
//! ## And the second: nothing is lost quietly
//!
//! Bytes go to the durable spool on any refusal and are never dropped.
//! [`spool::DurableSpool`] writes to disk before this handler ever answers
//! the client, so a killed daemon (or a killed laptop) loses at most the
//! narrow, documented exception in that module's doc -- not everything that
//! was in flight. That durability (#269) is what makes `daemon` safe to
//! become the default profile; #268 shipped with an accepted in-memory-only
//! gap this closes.

mod checkpoint;
mod classify;
mod drain;
mod forward;
mod mint;
mod normalize;
mod receive;
mod spool;

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::{Router, extract::State, http::StatusCode, routing::any};
use tokio::net::TcpListener;

use crate::{config::OauthConfig, otel_port};

/// Shared state for every request the daemon handles.
#[derive(Clone)]
struct DaemonState {
    http: reqwest::Client,
    config: OauthConfig,
    spool: Arc<Mutex<spool::DurableSpool>>,
}

/// Runs the daemon until a shutdown signal arrives.
///
/// Binds the fixed loopback port ([`otel_port::bind_loopback`]), which
/// refuses to fall back to an ephemeral port — a fallback would leave the
/// receiver where no client's telemetry can arrive. Runs on the accept loop
/// until SIGTERM/SIGINT, then drops the spool (accepted for #268; see the
/// module doc).
pub async fn serve(http: &reqwest::Client, config: &OauthConfig) -> Result<()> {
    config.otel_endpoint.as_deref().context(
        "no collector configured: supply --otel-endpoint / GOVERNANCE_AUTH_OTEL_ENDPOINT (or set \
         `otel_endpoint` in your config file) before running `serve --otel`",
    )?;

    let listener = otel_port::bind_loopback()?;
    // `bind_loopback` hands back a blocking std listener (its unit tests accept
    // on it with std threads); tokio requires a nonblocking socket before
    // `from_std`, and `from_std` panics on a blocking one, so flip the flag here
    // at the one and only adoption site.
    listener
        .set_nonblocking(true)
        .context("setting the OTEL loopback listener nonblocking for tokio")?;
    let listener = TcpListener::from_std(listener)
        .context("adopting the bound OTEL loopback listener into tokio")?;

    let state = DaemonState {
        http: http.clone(),
        config: config.clone(),
        spool: Arc::new(Mutex::new(
            spool::DurableSpool::open().context("opening the daemon's durable spool")?,
        )),
    };

    // Keeps retrying independent of client traffic -- see `drain::pump`'s
    // doc for why the opportunistic drain in `handle_request` alone is not
    // enough for AC1/AC2. Aborted below once the server itself stops; a
    // detached task would otherwise outlive the listener with nothing left
    // to hand its results to.
    let pump = tokio::spawn(drain::pump(state.clone()));

    let router = Router::new()
        .fallback(any(handle_request))
        .with_state(state);

    let result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("running the OTEL loopback receiver");
    pump.abort();
    result
}

/// Handles one OTLP request: receive -> classify -> mint -> normalize ->
/// forward, with fail-closed spooling on every refusal.
async fn handle_request(
    State(state): State<DaemonState>,
    request: axum::extract::Request,
) -> StatusCode {
    // Best-effort: drain any retained payloads before handling the new one.
    drain::drain_retained(&state).await;

    let incoming = match receive::build(request).await {
        Ok(incoming) => incoming,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE,
    };
    // The path is carried, not an admission gate (A2): log it for diagnostics,
    // never branch on it.
    tracing::trace!(method = %incoming.method, path = %incoming.path, "received OTLP");
    // Classify before mint so a mint refusal can still retain with the signal it
    // would have forwarded on (the spool stores `(Signal, bytes)`).
    let signal = classify::signal(&incoming.body, &incoming.path);
    let body = incoming.body;

    let minted = match mint::mint(&state.http, &state.config).await {
        Ok(minted) => minted,
        Err(error) => {
            tracing::warn!(error = %error, "no session; retaining payload, refusing to forward unauthenticated");
            drain::retain(&state, signal, body);
            return StatusCode::ACCEPTED;
        }
    };

    let stamped = match normalize::stamp(&body, &minted.access_token) {
        Ok(stamped) => stamped,
        Err(error) => {
            tracing::warn!(error = %error, "could not stamp identity; retaining original payload");
            drain::retain(&state, signal, body);
            return StatusCode::ACCEPTED;
        }
    };

    match forward::post(&state.http, &state.config, &minted.bearer, signal, &stamped).await {
        Ok(forward::Verdict::Accepted) => StatusCode::OK,
        Ok(forward::Verdict::Refused(status)) => {
            tracing::warn!(%status, "collector refused {signal}; retaining payload");
            drain::retain(&state, signal, stamped);
            StatusCode::ACCEPTED
        }
        Err(error) => {
            tracing::warn!(error = %error, "collector unreachable; retaining payload");
            drain::retain(&state, signal, stamped);
            StatusCode::ACCEPTED
        }
    }
}

/// Resolves once SIGINT or SIGTERM arrives, ending the accept loop.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %error, "failed to install Ctrl-C handler");
        }
    };
    #[cfg(unix)]
    let terminate = async {
        // SIGTERM is how systemd/launchd stop the daemon (#S3). If the handler
        // cannot be installed, SIGTERM's *default* action still terminates the
        // process, so there is nothing lost by not intercepting it -- await
        // forever and let the OS kill us, rather than panic inside the only
        // path that runs at shutdown time.
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                let _ = stream.recv().await;
            }
            Err(error) => {
                tracing::error!(error = %error, "failed to install the SIGTERM handler; relying on the OS default termination");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
