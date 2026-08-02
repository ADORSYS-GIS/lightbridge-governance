//! Entry point: an Envoy `ext_proc` server applying `governance-redact` to
//! gateway traffic (ADR-0116).
//!
//! Deployed as a sidecar in the gateway pod, not a standalone Deployment —
//! see the ADR for why. Two directions, two processing modes, deliberately
//! different: the request body arrives whole under `Buffered` mode, so it is
//! walked the same way `redact-gateway`'s request path did. The response
//! streams under `Streamed` mode; see `holdback` in `governance-redact` for
//! why buffering it whole is the wrong trade, and `service::response` for the
//! current limits of the streaming implementation.

mod config;
mod metrics;
mod service;

use std::sync::Arc;

use anyhow::Context;
use envoy_types::pb::envoy::service::ext_proc::v3::external_processor_server::ExternalProcessorServer;
use governance_redact::Engine;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cfg = config::Args::parse();

    let profile = config::resolve_profile(&cfg.redact_profile)?;
    let fail_closed = profile.fail_closed;
    let engine =
        Arc::new(Engine::new(profile, cfg.redact_hash_salt.clone()).context("building engine")?);

    tracing::info!(
        profile = engine.profile().name,
        fail_closed,
        detects_names = engine.profile().detects_names(),
        response_hold_back_bytes = cfg.response_hold_back_bytes,
        "redact-extproc starting"
    );
    if !engine.profile().detects_names() {
        tracing::warn!(
            "person-name detection is NOT active (no NER model); pattern and validator recognizers only"
        );
    }

    let metrics = Arc::new(metrics::Metrics::new().context("registering metrics")?);
    let metrics_addr = cfg.metrics_listen_addr;
    let metrics_for_http = Arc::clone(&metrics);
    tokio::spawn(async move {
        if let Err(e) = metrics::serve(metrics_addr, metrics_for_http).await {
            tracing::error!(error = %e, "metrics server exited");
        }
    });

    let svc = service::RedactProcessor::new(engine, metrics, cfg.response_hold_back_bytes);

    tracing::info!(listen_addr = %cfg.listen_addr, "listening");
    tonic::transport::Server::builder()
        .add_service(ExternalProcessorServer::new(svc))
        .serve(cfg.listen_addr)
        .await
        .context("ext_proc server")?;

    Ok(())
}
