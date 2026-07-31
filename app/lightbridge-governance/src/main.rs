//! The governance API: the read surface over the registry and both connectors'
//! normalized data, plus `/internal/v1/resolve` for Authorino and `/metrics` for
//! the ServiceMonitor.
//!
//! It also OWNS the connector operational metrics (ADR-0007): a CronJob pod
//! cannot be scraped, so the collector records run outcomes in `ingest_manifest`
//! and this always-running process derives `governance_connector_*` from them.

use anyhow::Result;
use clap::Parser;

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

    // TODO(RFC-0001/RFC-0002): mount the router once the registry lands.
    let _ = args.database_url;
    Ok(())
}
