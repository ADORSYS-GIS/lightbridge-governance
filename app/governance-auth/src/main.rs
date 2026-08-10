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
mod otel;
mod redacted;
mod security;
mod update;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use config::OauthConfigArgs;

#[derive(Debug, Parser)]
#[command(
    name = "governance-auth",
    version,
    about = "OAuth2 credential helper for pointing Claude Code / Codex at this org's gateway."
)]
struct Cli {
    #[command(flatten)]
    oauth: OauthConfigArgs,

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
    /// Print OTLP export headers as a JSON object -- the format Claude
    /// Code's `otelHeadersHelper` requires. Same refresh-or-fail-closed
    /// behaviour as `token`; this is that token wrapped in the shape the
    /// hook expects, so telemetry auth refreshes automatically instead of
    /// depending on anyone rotating a long-lived key by hand.
    OtelHeaders,
    /// Re-apply the OpenTelemetry configuration to Claude Code and Codex
    /// without re-running the interactive login. `login` already does this;
    /// this is for an existing session whose endpoint or ingest token
    /// changed, and it's the command to re-run after installing one of the
    /// two tools for the first time.
    Configure,
    /// Print whether a cached session exists and its freshness.
    Status,
    /// Remove the cached session.
    Logout,
    /// Replace this binary with the latest GitHub release for this platform.
    ///
    /// The download is checksummed against the release's own `.sha256`, which
    /// catches corruption but is NOT a signature -- see `crate::update`'s
    /// module doc for the trust model and why it says "checksummed", not
    /// "verified".
    SelfUpdate {
        /// Report whether an update exists and exit, changing nothing.
        #[arg(long)]
        check: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    // Clap can't enforce "issuer/client-id must be present" itself once
    // they're `global` (see `OauthConfigArgs`'s doc comment) -- do it here,
    // before anything else runs, so a missing flag/env var is reported the
    // same way a clap parse error would be: immediately, on stderr, nonzero
    // exit, no partial work.
    let oauth = cli.oauth.resolve().map_err(|error| anyhow!(error))?;
    let http = reqwest::Client::builder()
        // `--issuer`/discovery already reject an insecure *initial* request
        // URL (config::parse_issuer, oauth::discovery::require_same_origin)
        // -- this redirect policy is the other half: it re-checks every hop
        // of every redirect chain, so a same-origin HTTPS request can't be
        // walked down to plaintext HTTP by a 3xx response from a
        // compromised or misconfigured server. Defence in depth, not
        // redundant: this is the only layer that sees a redirect target.
        .redirect(security::redirect_policy())
        .build()
        .context("building the HTTP client")?;

    match cli.command {
        Command::Login { device_code } => oauth::login(&http, &oauth, device_code).await,
        Command::Token => oauth::token(&http, &oauth).await,
        Command::OtelHeaders => oauth::otel_headers(&http, &oauth).await,
        Command::Configure => oauth::configure(&oauth),
        Command::Status => oauth::status(&oauth),
        Command::Logout => oauth::logout(&oauth),
        Command::SelfUpdate { check } => update::run(&http, check).await,
    }
}
