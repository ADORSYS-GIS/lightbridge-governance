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
//! Bytes go to the in-memory spool on a *retryable* refusal (an unreachable
//! collector, a mint failure) and are never silently dropped: a failed
//! `retain` answers `503`, never a `202` the payload never earned (#290
//! review, P1-3). A *permanent* refusal (`is_permanent`: 400/413/422) is the
//! one deliberate exception -- discarded and logged loudly rather than
//! retained forever, because retrying it can never succeed (P2-6) and doing
//! so anyway would fill the spool with payloads that can never drain. The
//! spool is in-memory and lost on process exit — **that is accepted for #268
//! only** because durability (#S2) is a separate story that must land before
//! the daemon becomes the default profile.

mod classify;
mod drain;
mod forward;
mod mint;
mod normalize;
mod receive;
mod shutdown;
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
    spool: Arc<Mutex<spool::Spool>>,
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
        spool: Arc::new(Mutex::new(spool::Spool::new())),
    };

    let router = Router::new()
        .fallback(any(handle_request))
        .with_state(state);

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown::signal())
        .await
        .context("running the OTEL loopback receiver")
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
        // Refused before the body was ever read (P1-2, #290 review): an
        // untrusted `Host` or a CORS-simple `Content-Type` -- see
        // `receive`'s module doc for why both are admission gates.
        Err(receive::ReceiveError::UntrustedHost) => {
            tracing::warn!("refusing a request with an untrusted Host header");
            return StatusCode::FORBIDDEN;
        }
        Err(receive::ReceiveError::UnsupportedContentType) => {
            return StatusCode::UNSUPPORTED_MEDIA_TYPE;
        }
        Err(receive::ReceiveError::Body(error)) => {
            tracing::warn!(error = %error, "could not read the request body");
            return StatusCode::PAYLOAD_TOO_LARGE;
        }
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
            return retained_status(&state, signal, body);
        }
    };

    let stamped = match normalize::stamp(&body, &minted.access_token) {
        Ok(stamped) => stamped,
        Err(error) => {
            tracing::warn!(error = %error, "could not stamp identity; retaining original payload");
            return retained_status(&state, signal, body);
        }
    };

    match forward::post(&state.http, &state.config, &minted.bearer, signal, &stamped).await {
        Ok(forward::Verdict::Accepted) => StatusCode::OK,
        // A permanent refusal will never succeed no matter how many times it
        // is offered (`is_permanent`'s whole point, #290 review P2-6) --
        // propagate the collector's own status rather than either retaining
        // it forever or lying with a `202` the payload never earned.
        Ok(forward::Verdict::Refused(status)) => {
            tracing::error!(%status, "collector permanently refused {signal}; discarding, not retained");
            status
        }
        Err(error) => {
            tracing::warn!(error = %error, "collector unreachable; retaining payload");
            retained_status(&state, signal, stamped)
        }
    }
}

/// Retains `payload` and answers the status that actually describes what
/// happened: `202` when it is durably queued for retry, `503` when the spool
/// itself is full and the payload was **not** retained (#290 review, P1-3) --
/// answering `202` in that case would tell the exporter "delivered" while its
/// only copy was dropped, the unavailable branch becoming the permissive one.
fn retained_status(
    state: &DaemonState,
    signal: crate::copilot::Signal,
    payload: Vec<u8>,
) -> StatusCode {
    if drain::retain(state, signal, payload) {
        StatusCode::ACCEPTED
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
