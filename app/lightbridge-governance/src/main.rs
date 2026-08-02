//! The governance API: the read surface over the registry and both connectors'
//! normalized data, plus `/internal/v1/resolve` for Authorino and `/metrics` for
//! the ServiceMonitor.
//!
//! It also OWNS the connector operational metrics (ADR-0007): a CronJob pod
//! cannot be scraped, so the collector records run outcomes in `ingest_manifest`
//! and this always-running process derives `governance_connector_*` from them.

mod router;

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
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt().json().init();

    let args = Args::parse();
    tracing::info!(listen_addr = %args.listen_addr, "lightbridge-governance starting");

    let pool = cratestack::sqlx::PgPool::connect(&args.database_url).await?;
    let db = Cratestack::builder(pool).build();
    let app = router::build_router(db);

    let listener = tokio::net::TcpListener::bind(&args.listen_addr).await?;
    tracing::info!(listen_addr = %args.listen_addr, "listening");
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}
