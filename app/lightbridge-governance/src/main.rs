//! The governance API: the read surface over the registry and both connectors'
//! normalized data, plus `/internal/v1/resolve` for Authorino, `/internal/v1/ingest`
//! for OTLP telemetry, and `/metrics` for the ServiceMonitor.
//!
//! It also OWNS the connector operational metrics (ADR-0007): a CronJob pod
//! cannot be scraped, so the collector records run outcomes in `ingest_manifest`
//! and this always-running process derives `governance_connector_*` from them.

mod args;
mod authn;
mod ingest;
mod metrics;
mod rate_limit;
mod resolve;
mod router;

use std::sync::Arc;

use anyhow::Result;
use args::Args;
use clap::Parser;
use governance_core::schema::cratestack_schema::Cratestack;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt().json().init();

    let args = Args::parse();
    tracing::info!(listen_addr = %args.listen_addr, "lightbridge-governance starting");

    let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
    let verifier = authn::TokenReviewVerifier::new(
        args.kube_apiserver_url,
        vec![args.token_review_audience],
        args.allowed_service_accounts,
    )
    .map_err(|e| anyhow::anyhow!("TokenReview verifier init failed: {e}"))?;
    let resolve_state = resolve::ResolveState {
        pool: pool.clone(),
        verifier,
        resolve_timeout: std::time::Duration::from_millis(args.resolve_timeout_ms),
        cache: resolve::build_cache(
            std::time::Duration::from_secs(args.resolve_cache_ttl_secs),
            args.resolve_cache_max_capacity,
        ),
    };
    let db = Cratestack::builder(pool.clone()).build();
    let metrics = Arc::new(metrics::Metrics::new());
    // Cloned before `pool` moves into `ingest_state` below -- `PgPool` is
    // Arc-backed, so this is a refcount bump, not a new connection pool.
    let connector_metrics_pool = pool.clone();
    let tenant_id: Arc<str> = Arc::from(args.tenant_id.as_str());
    let connector_metrics_timeout =
        std::time::Duration::from_millis(args.connector_metrics_timeout_ms);
    let org_kpi_timeout = std::time::Duration::from_millis(args.org_kpi_timeout_ms);
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
                    let pool = connector_metrics_pool.clone();
                    let tenant_id = Arc::clone(&tenant_id);
                    async move {
                        // Refresh-on-scrape, bounded by a timeout well under
                        // the ServiceMonitor's own interval -- see
                        // metrics.rs's module doc comment for the tradeoff.
                        metrics
                            .refresh_connector_freshness(
                                &pool,
                                &tenant_id,
                                connector_metrics_timeout,
                            )
                            .await;
                        // Org-level KPI gauges (ADR-0003's bounded exception)
                        // -- same refresh-on-scrape shape, independent
                        // timeout budget.
                        metrics
                            .refresh_org_kpis(&pool, &tenant_id, org_kpi_timeout)
                            .await;
                        metrics.render()
                    }
                }),
            ),
    );

    let listener = tokio::net::TcpListener::bind(&args.listen_addr).await?;
    tracing::info!(listen_addr = %args.listen_addr, "listening");
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}
