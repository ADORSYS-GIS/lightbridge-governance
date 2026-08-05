//! OAuth2 credential helper for Claude Code and Codex against this org's
//! Keycloak-protected AI gateway. Not a server -- Keycloak already is the
//! authorization server and the gateway already validates its JWTs. This is
//! a pure OAuth2 *client*: `login` performs the interactive flow once,
//! `token` prints a currently-valid access token on every subsequent call,
//! wired into Claude Code's `apiKeyHelper` and Codex's
//! `[model_providers.<id>.auth] command`.
//!
//! All UX, prompts and errors go to stderr. `token`'s stdout carries the
//! access token and nothing else, ever.

mod browser;
mod cache;
mod config;
mod oauth;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::OauthConfig;

#[derive(Debug, Parser)]
#[command(
    name = "governance-auth",
    version,
    about = "OAuth2 credential helper for pointing Claude Code / Codex at this org's gateway."
)]
struct Cli {
    #[command(flatten)]
    oauth: OauthConfig,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Interactive first-time login: opens a browser (or, with
    /// `--device-code`, prints a verification URL and polls) and caches the
    /// resulting session.
    Login {
        /// Use the device-authorization flow instead of the loopback
        /// browser flow. For headless sessions (SSH, cloud dev boxes) with
        /// no local browser to open.
        #[arg(long)]
        device_code: bool,
    },
    /// Print a currently-valid access token to stdout -- nothing else on
    /// stdout, ever. This is the command to wire into `apiKeyHelper` /
    /// `auth.command`. Fails closed and non-interactively if there's no
    /// valid session.
    Token,
    /// Print whether a cached session exists and its freshness.
    Status,
    /// Remove the cached session.
    Logout,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let http = reqwest::Client::builder()
        .build()
        .context("building the HTTP client")?;

    match cli.command {
        Command::Login { device_code } => oauth::login(&http, &cli.oauth, device_code).await,
        Command::Token => oauth::token(&http, &cli.oauth).await,
        Command::Status => oauth::status(&cli.oauth),
        Command::Logout => oauth::logout(&cli.oauth),
    }
}
