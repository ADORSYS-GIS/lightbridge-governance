//! OAuth2 credential helper for Claude Code, Codex and VS Code Copilot against
//! this org's AI gateway. Not a server -- an OIDC-compliant authorization
//! server (today, Keycloak; nothing here assumes it -- see `crate::config`'s
//! module doc) already validates its own tokens, and the gateway already
//! validates the JWTs it issues. This is a pure OAuth2 *client*: `login`
//! performs the interactive flow once, `token` prints a currently-valid access
//! token on every subsequent call, wired into Claude Code's `apiKeyHelper` and
//! Codex's `[model_providers.<id>.auth] command`.
//!
//! The command tree, and the rule that decided which names were allowed to
//! move when it was reorganised into scopes, live in `crate::cli`.
//!
//! Optionally, `token`/`otel headers` can exchange that access token (RFC
//! 8693) for one minted by a second, downstream authorization server before
//! printing it -- OFF by default, see `crate::config::ExchangeConfig` and
//! `oauth::exchange`.
//!
//! All UX, prompts and errors go to stderr, and a durable copy of the
//! diagnostics goes to a rotating file (`crate::logging`). `token`'s stdout
//! carries the access token and nothing else, ever.

mod browser;
mod cache;
mod cli;
mod config;
mod config_file;
mod config_persist;
mod copilot;
mod dashboard;
mod freshness;
mod logging;
mod managed;
mod oauth;
mod optout;
mod otel;
mod otel_port;
mod redacted;
mod schedule;
mod security;
mod templates;
mod update;
mod vscode;

use anyhow::{Context, Result};
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    logging::init();
    let cli = cli::Cli::parse();
    logging::finish(cli.run(&http_client()?).await)
}

/// The one HTTP client every command shares.
fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        // ⚠️ Without these, a server that accepts the connection and then says
        // nothing blocks the process for ever. That is not a lost wake: the
        // drain holds `copilot-push.lock` across its POSTs, so one stuck
        // `copilot push` wedges every later one behind it, and the sample
        // systemd unit (`Type=oneshot`) defaults `TimeoutStartSec=` to
        // infinity so nothing kills it either. Measured: a healthy collector
        // received zero requests from the wake after a stuck one.
        //
        // A READ timeout, not a total `timeout()`: read_timeout resets after
        // every successful read, so it catches a silent peer without putting a
        // deadline on a large body. `self update` streams a release binary
        // over this same client and a total deadline would fail that on a slow
        // link.
        //
        // 15s is half again the OpenTelemetry SDKs' own default OTLP exporter
        // timeout, and the cost of it firing early is asymmetric in the safe
        // direction: a timed-out POST advances nothing, so the bytes stay
        // pending and go again on the next wake. Nothing is ever lost by
        // being impatient here.
        .connect_timeout(std::time::Duration::from_secs(10))
        .read_timeout(std::time::Duration::from_secs(15))
        // `--issuer`/discovery already reject an insecure *initial* request
        // URL (config::parse_issuer, oauth::discovery::require_same_origin)
        // -- this redirect policy is the other half: it re-checks every hop
        // of every redirect chain, so a same-origin HTTPS request can't be
        // walked down to plaintext HTTP by a 3xx response from a
        // compromised or misconfigured server. Defence in depth, not
        // redundant: this is the only layer that sees a redirect target.
        .redirect(security::redirect_policy())
        .build()
        .context("building the HTTP client")
}
