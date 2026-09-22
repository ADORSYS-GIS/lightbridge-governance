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
mod request;
mod shutdown;
mod signal;
mod source_stamp;
mod spool;
mod spool_compaction;
mod status;

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::{Router, routing::any};
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
        .fallback(any(request::handle_request))
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
