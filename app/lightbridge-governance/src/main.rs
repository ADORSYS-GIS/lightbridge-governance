//! The governance API: the read surface over the registry and both connectors'
//! normalized data, plus `/internal/v1/resolve` for Authorino and `/metrics`
//! for the ServiceMonitor.
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

    /// TTL for the in-process `/internal/v1/resolve` cache (ADR-0006,
    /// ADR-0007). This *is* the revocation SLA the runbook documents --
    /// `docs/runbooks/revoke-an-integration-token.md` -- so it must match
    /// `config/default.yaml`'s `resolveCache.ttlSeconds` default. Has a
    /// `default_value_t` deliberately: an env var with no default is how
    /// `INTERNAL_INGEST_TOKEN` (since removed) became a CrashLoopBackOff (see
    /// AGENTS.md); this one must never repeat that.
    #[arg(long, env = "RESOLVE_CACHE_TTL_SECS", default_value_t = 60)]
    resolve_cache_ttl_secs: u64,

    /// Max entries the `/internal/v1/resolve` cache holds before moka starts
    /// evicting. Must match `config/default.yaml`'s
    /// `resolveCache.maxCapacity` default. See `resolve_cache_ttl_secs` for
    /// why this also carries a `default_value_t`.
    #[arg(long, env = "RESOLVE_CACHE_MAX_CAPACITY", default_value_t = 10_000)]
    resolve_cache_max_capacity: u64,

    /// Single-tenant deployment (ADR-0001). Scopes the `governance_connector_*`
    /// freshness query `/metrics` derives from `ingest_manifests` (ADR-0007) --
    /// `tenant_id` belongs in the WHERE clause of every query, even in a
    /// deployment that only ever has the one. No default: an empty tenant_id
    /// has no safe meaning here, matching `governance-ctl`'s own `TENANT_ID`
    /// (`app/governance-ctl/src/sync.rs`).
    ///
    /// `value_parser` rather than a bare `String`: clap treats a required arg
    /// as satisfied by an EMPTY value, and the chart renders `TENANT_ID: ""`
    /// by default, so `required` alone never caught the case that actually
    /// happened in production -- a deployment running with no tenant identity
    /// at all, scoping `governance_connector_*` to a tenant that owns no rows
    /// and therefore reporting has_synced=0 forever.
    #[arg(long, env = "TENANT_ID", value_parser = parse_tenant_id)]
    tenant_id: String,

    /// Upper bound on the `governance_connector_*` freshness query `/metrics`
    /// runs against `ingest_manifests` on every scrape (ADR-0007).
    /// Deliberately far below the ServiceMonitor's 30s scrape interval
    /// (`charts/lightbridge-governance/values.yaml`'s `serviceMonitor.interval`),
    /// same reasoning as `resolve_timeout_ms`: a dependency's own timeout must
    /// be shorter than the caller's, not left at sqlx's 30s pool
    /// `acquire_timeout` default.
    #[arg(long, env = "CONNECTOR_METRICS_TIMEOUT_MS", default_value_t = 3_000)]
    connector_metrics_timeout_ms: u64,

    /// Upper bound (per query -- usage and seats are queried independently,
    /// see `Metrics::refresh_org_kpis`) on the `governance_org_*` KPI
    /// queries `/metrics` runs against `copilot_org_dailys`/
    /// `copilot_seat_snapshots` on every scrape. Same reasoning as
    /// `connector_metrics_timeout_ms`.
    #[arg(long, env = "ORG_KPI_TIMEOUT_MS", default_value_t = 3_000)]
    org_kpi_timeout_ms: u64,
}

/// Rejects an empty or whitespace-only tenant id at argument-parse time, so a
/// misconfigured deployment fails immediately and loudly instead of running
/// with no tenant identity.
///
/// This is a real failure that reached production, not a hypothetical: the
/// chart's structural default is `TENANT_ID: ""`, clap counts a required arg
/// as satisfied by an empty value, and `std::env::var` returns `Ok("")` for a
/// set-but-empty variable -- so all three layers said "present" and the
/// deployment ran with `tenant_id = ''`.
fn parse_tenant_id(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(
            "TENANT_ID is set but empty. It is this deployment's tenant identity \
             (ADR-0001) and scopes the governance_connector_* freshness query \
             /metrics derives from ingest_manifests (ADR-0007), so an empty value \
             reports has_synced=0 forever regardless of how healthy the connector \
             is. Set `copilot.tenantId` in the deployed values (ai-helm-values)."
                .to_owned(),
        );
    }
    Ok(trimmed.to_owned())
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
    let tenant_id: Arc<str> = Arc::from(args.tenant_id.as_str());
    let connector_metrics_timeout =
        std::time::Duration::from_millis(args.connector_metrics_timeout_ms);
    let org_kpi_timeout = std::time::Duration::from_millis(args.org_kpi_timeout_ms);

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
                    let pool = pool.clone();
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
