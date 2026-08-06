//! The governance API: the read surface over the registry and both connectors'
//! normalized data, plus `/internal/v1/resolve` for Authorino, `/internal/v1/ingest`
//! for OTLP telemetry, and `/metrics` for the ServiceMonitor.
//!
//! It also OWNS the connector operational metrics (ADR-0007): a CronJob pod
//! cannot be scraped, so the collector records run outcomes in `ingest_manifest`
//! and this always-running process derives `governance_connector_*` from them.

mod ingest;
mod metrics;
mod rate_limit;
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

    /// Shared secret the OpenTelemetry Collector presents as
    /// `X-Internal-Token` on `/internal/v1/ingest` (#30). Never logged --
    /// only its presence/absence is, via the request outcome.
    #[arg(long, env = "INTERNAL_INGEST_TOKEN")]
    internal_ingest_token: String,

    /// Upper bound on `/internal/v1/resolve`'s credential lookup, in
    /// milliseconds. Deliberately far below sqlx's own 30s pool default --
    /// this is Authorino's ext_authz hot path, and a dependency's own
    /// timeout must be shorter than the caller's (ADR-0006).
    #[arg(long, env = "RESOLVE_TIMEOUT_MS", default_value_t = 500)]
    resolve_timeout_ms: u64,

    /// TTL for the in-process `/internal/v1/resolve` cache (ADR-0006,
    /// ADR-0007). This *is* the revocation SLA the runbook documents --
    /// `docs/runbooks/revoke-an-integration-token.md` -- so it must match
    /// `config/default.yaml`'s `resolveCache.ttlSeconds` default. Has a
    /// `default_value_t` deliberately: an env var with no default is how the
    /// `INTERNAL_INGEST_TOKEN` chart gap became a CrashLoopBackOff (see
    /// AGENTS.md); this one must never repeat that.
    #[arg(long, env = "RESOLVE_CACHE_TTL_SECS", default_value_t = 60)]
    resolve_cache_ttl_secs: u64,

    /// Max entries the `/internal/v1/resolve` cache holds before moka starts
    /// evicting. Must match `config/default.yaml`'s
    /// `resolveCache.maxCapacity` default. See `resolve_cache_ttl_secs` for
    /// why this also carries a `default_value_t`.
    #[arg(long, env = "RESOLVE_CACHE_MAX_CAPACITY", default_value_t = 10_000)]
    resolve_cache_max_capacity: u64,

    /// Max `/internal/v1/ingest` requests per integration per
    /// `INGEST_RATE_WINDOW_SECS`. A throttle, not a billing meter.
    #[arg(long, env = "INGEST_RATE_MAX_PER_WINDOW", default_value_t = 600)]
    ingest_rate_max_per_window: u64,

    /// Fixed window length for the `/internal/v1/ingest` rate limiter.
    #[arg(long, env = "INGEST_RATE_WINDOW_SECS", default_value_t = 60)]
    ingest_rate_window_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt().json().init();

    let args = Args::parse();
    tracing::info!(listen_addr = %args.listen_addr, "lightbridge-governance starting");

    let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
    let internal_token: Arc<str> = Arc::from(args.internal_resolve_token.as_str());
    let resolve_state = resolve::ResolveState {
        pool: pool.clone(),
        internal_token: internal_token.clone(),
        resolve_timeout: std::time::Duration::from_millis(args.resolve_timeout_ms),
        cache: resolve::build_cache(
            std::time::Duration::from_secs(args.resolve_cache_ttl_secs),
            args.resolve_cache_max_capacity,
        ),
    };
    let db = Cratestack::builder(pool.clone()).build();
    let metrics = Arc::new(metrics::Metrics::new());
    let ingest_state = ingest::IngestState {
        pool,
        internal_token: Arc::from(args.internal_ingest_token.as_str()),
        rate_limiter: Arc::new(rate_limit::RateLimiter::new(
            args.ingest_rate_max_per_window,
            args.ingest_rate_window_secs,
        )),
        metrics: metrics.clone(),
    };

    let app = router::build_router(db).merge(
        axum::Router::new()
            .route(
                "/internal/v1/resolve",
                axum::routing::post(resolve::resolve),
            )
            .with_state(resolve_state)
            .route("/internal/v1/ingest", axum::routing::post(ingest::ingest))
            // Deliberate body cap for the OTLP export batch: axum's implicit
            // default is 2 MiB; agent telemetry batches can be larger, so the
            // limit is raised and made explicit rather than left at an
            // undocumented default that silently drops oversize payloads with
            // a 413 (a permanent, non-retried failure for the collector).
            .layer(axum::extract::DefaultBodyLimit::max(
                ingest::MAX_OTLP_BODY_BYTES,
            ))
            .with_state(ingest_state)
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
