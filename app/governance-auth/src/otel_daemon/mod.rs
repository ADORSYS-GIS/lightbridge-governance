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
//! A *retryable* refusal (an unreachable collector, a mint failure) goes to
//! the durable spool, never silently dropped: a failed `retain` answers
//! `503`, never a `202` the payload never earned. A *permanent* refusal
//! (400/413/422) is discarded and logged loudly instead, because retrying it
//! can never succeed. [`spool::DurableSpool`] writes to disk, `fsync`-durably,
//! before this handler ever answers the client (#269), so a killed daemon --
//! or laptop -- loses at most the narrow exception that module's doc names.

mod checkpoint;
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
///
/// `config` is `Arc`-wrapped: axum's `State<S>` clones `S` per request, and
/// cloning the whole `OauthConfig` was a fresh allocation per field on every
/// request; `Arc::clone` is a refcount bump.
#[derive(Clone)]
struct DaemonState {
    http: reqwest::Client,
    config: Arc<OauthConfig>,
    spool: Arc<Mutex<spool::DurableSpool>>,
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
    };

    // Keeps retrying independent of client traffic -- see `drain::pump`'s
    // doc. Aborted below once the server itself stops; a detached task
    // would otherwise outlive the listener with nothing to hand results to.
    let pump = tokio::spawn(drain::pump(state.clone()));

    let router = Router::new()
        .fallback(any(handle_request))
        .with_state(state);

    let result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown::signal())
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
    // Admission FIRST: `receive::build`'s `Host`/`Content-Type` checks make
    // an untrusted request free only if nothing costly runs before them.
    // `drain_retained` mints and can POST to the real collector, so it must
    // never run before admission -- an untrusted caller could otherwise
    // force credentialed work on demand, once per rejected request.
    let incoming = match receive::build(request).await {
        Ok(incoming) => incoming,
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

    // Best-effort: `drain_retained` only mints when the spool is non-empty.
    drain::drain_retained(&state).await;

    // Carried for diagnostics, never a branch (A2).
    tracing::trace!(method = %incoming.method, path = %incoming.path, "received OTLP");
    // Parsed once, threaded through classify/normalize/forward.
    let parsed: Option<serde_json::Value> = serde_json::from_slice(&incoming.body).ok();
    let is_json = parsed.is_some();
    // Classify before mint: a mint refusal can still retain with this signal.
    let signal = classify::signal(parsed.as_ref(), &incoming.path);
    let body = incoming.body;

    let minted = match mint::mint(&state.http, &state.config).await {
        Ok(minted) => minted,
        Err(error) => {
            tracing::warn!(error = %error, "no session; retaining payload, refusing to forward unauthenticated");
            return retained_status(&state, signal, body).await;
        }
    };

    // `None`: a live pass-through has no stable key yet -- see `normalize`.
    let stamped = match normalize::stamp(parsed, &body, &minted.access_token, None) {
        Ok(stamped) => stamped,
        Err(error) => {
            tracing::warn!(error = %error, "could not stamp identity; retaining original payload");
            return retained_status(&state, signal, body).await;
        }
    };

    match forward::post(
        &state.http,
        &state.config,
        &minted.bearer,
        signal,
        &stamped,
        is_json,
    )
    .await
    {
        Ok(forward::Verdict::Accepted) => StatusCode::OK,
        // A permanent refusal will never succeed no matter how many times it
        // is offered -- propagate the collector's own status rather than
        // retaining it forever or lying with a `202` it never earned.
        Ok(forward::Verdict::Refused(status)) => {
            tracing::error!(%status, "collector permanently refused {signal}; discarding, not retained");
            status
        }
        Err(error) => {
            tracing::warn!(error = %error, "collector unreachable; retaining payload");
            retained_status(&state, signal, stamped).await
        }
    }
}

/// Retains `payload` and answers the status that actually describes what
/// happened: `202` when it is durably queued for retry, `503` when the spool
/// itself is full and the payload was **not** retained -- answering `202`
/// there would tell the exporter "delivered" while its only copy was
/// dropped, the unavailable branch becoming the permissive one.
async fn retained_status(
    state: &DaemonState,
    signal: crate::copilot::Signal,
    payload: Vec<u8>,
) -> StatusCode {
    if drain::retain(state, signal, payload).await {
        StatusCode::ACCEPTED
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
