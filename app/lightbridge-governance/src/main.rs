//! The governance API: the read surface over the registry and both connectors'
//! normalized data, plus `/internal/v1/resolve` for Authorino and `/metrics` for
//! the ServiceMonitor.
//!
//! It also OWNS the connector operational metrics (ADR-0007): a CronJob pod
//! cannot be scraped, so the collector records run outcomes in `ingest_manifest`
//! and this always-running process derives `governance_connector_*` from them.

mod metrics;
mod resolve;
mod router;

use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use governance_core::schema::cratestack_schema::Cratestack;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Command-line surface for the API server.
#[derive(Debug, Parser)]
#[command(name = "lightbridge-governance", version, about)]
struct Args {
    /// Address to bind the HTTP listener to.
    #[arg(long, env = "LISTEN_ADDR", default_value = "0.0.0.0:8080")]
    listen_addr: String,

    /// Postgres connection string.
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,

    /// Shared secret Authorino's `metadata.http` step presents as
    /// `X-Internal-Token` on `/internal/v1/resolve` (#11, ADR-0006). Never
    /// logged -- only its presence/absence is, via the request outcome.
    #[arg(long, env = "INTERNAL_RESOLVE_TOKEN")]
    internal_resolve_token: String,

    /// Upper bound on `/internal/v1/resolve`'s credential lookup, in
    /// milliseconds. Deliberately far below sqlx's own 30s pool default --
    /// this is Authorino's ext_authz hot path, and a dependency's own
    /// timeout must be shorter than the caller's (ADR-0006).
    #[arg(long, env = "RESOLVE_TIMEOUT_MS", default_value_t = 500)]
    resolve_timeout_ms: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt().json().init();

    let args = Args::parse();
    tracing::info!(listen_addr = %args.listen_addr, "lightbridge-governance starting");

    let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
    let resolve_state = resolve::ResolveState {
        pool: pool.clone(),
        internal_token: Arc::from(args.internal_resolve_token),
        resolve_timeout: std::time::Duration::from_millis(args.resolve_timeout_ms),
    };
    let db = Cratestack::builder(pool).build();
    let metrics = Arc::new(metrics::Metrics::new());
    let app = router::build_router(db).merge(
        axum::Router::new()
            .route(
                "/internal/v1/resolve",
                axum::routing::post(resolve::resolve),
            )
            .with_state(resolve_state)
            // Health and metrics are unauthenticated and deliberately outside
            // the registry/resolve auth paths, so an orchestrator can probe a
            // service that is otherwise refusing traffic.
            .route("/livez", axum::routing::get(|| async { "ok" }))
            .route("/readyz", axum::routing::get(|| async { "ok" }))
            .route(
                "/metrics",
                axum::routing::get(move || {
                    let metrics = Arc::clone(&metrics);
                    async move { metrics.render() }
                }),
            ),
    );

    let listener = tokio::net::TcpListener::bind(&args.listen_addr).await?;
    tracing::info!(listen_addr = %args.listen_addr, "listening");
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}
