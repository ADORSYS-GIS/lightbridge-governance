//! An OpenAI-compatible redaction proxy.
//!
//! Sits in front of the AI gateway and scans prompts on the way out and model
//! output on the way back:
//!
//! ```text
//! client -> redact-gateway -> core-gateway-internal (authorino -> AIEG -> provider)
//! ```
//!
//! A front proxy rather than an Envoy `ext_proc` filter, so there is no
//! filter-chain ordering dependency and no fork of the AI Gateway — see
//! ai-helm ADR-0113.
//!
//! # This service authenticates nobody
//!
//! The caller's `Authorization` header is forwarded upstream untouched, and the
//! upstream gateway performs authentication exactly as it would without this
//! proxy in the path. This binary holds no credential of its own, so a
//! compromise of it yields no token. Trust is network-level: ClusterIP plus a
//! `CiliumNetworkPolicy`, matching the existing internal-plane model.

mod config;
mod metrics;
mod proxy;

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Router,
    routing::{get, post},
};
use clap::Parser;
use governance_redact::Engine;

use crate::{
    config::{Args, Config},
    metrics::Metrics,
    proxy::AppState,
};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt().json().init();

    let cfg = Config::from_args(Args::parse())?;

    let engine = Engine::new(cfg.profile.clone(), &cfg.hash_salt)
        .context("building the redaction engine")?;

    tracing::info!(
        profile = cfg.profile.name,
        fail_closed = cfg.profile.fail_closed,
        detects_names = cfg.profile.detects_names(),
        provider = %cfg.provider_base_url,
        "redact-gateway starting"
    );
    if !cfg.profile.detects_names() {
        // Said once, loudly, at startup: nobody should discover this from a
        // dashboard that looks clean.
        tracing::warn!(
            "person-name detection is NOT active (no NER model); \
             pattern and validator recognizers only"
        );
    }

    let client = build_client(&cfg).context("building the upstream HTTP client")?;

    let state = Arc::new(AppState {
        engine,
        client,
        provider_base_url: cfg.provider_base_url.clone(),
        metrics: Metrics::new().context("registering metrics")?,
    });

    let app = Router::new()
        // Health and metrics are unauthenticated and deliberately outside the
        // proxy path, so an orchestrator can probe a service that is refusing
        // traffic.
        .route("/livez", get(|| async { "ok" }))
        .route("/readyz", get(|| async { "ok" }))
        .route(
            "/metrics",
            get({
                let state = Arc::clone(&state);
                move || {
                    let state = Arc::clone(&state);
                    async move { state.metrics.render() }
                }
            }),
        )
        .route("/v1/chat/completions", post(proxy::handle))
        .route("/v1/completions", post(proxy::handle))
        .route("/v1/embeddings", post(proxy::handle))
        .layer(axum::extract::DefaultBodyLimit::max(cfg.max_body_bytes))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&cfg.listen_addr)
        .await
        .with_context(|| format!("binding {}", cfg.listen_addr))?;
    tracing::info!(listen_addr = %cfg.listen_addr, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Builds the upstream HTTP client.
///
/// ⚠️ The internal CA is added explicitly via [`reqwest::Certificate`]. This
/// client is **rustls**, and rustls does not read `SSL_CERT_FILE` — that is an
/// OpenSSL convention. The service this replaces was OpenSSL-linked and got its
/// trust that way; carrying that assumption over would fail at connect time,
/// in production, on the first request.
fn build_client(cfg: &Config) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(cfg.provider_timeout_secs));

    if let Some(pem) = &cfg.provider_ca_pem {
        let cert = reqwest::Certificate::from_pem(pem).context("parsing the provider CA PEM")?;
        builder = builder.add_root_certificate(cert);
        tracing::info!("added an internal CA to the upstream trust store");
    }

    builder.build().context("building reqwest client")
}
