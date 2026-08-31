//! The OTLP/HTTP POST.
//!
//! One rule dominates this file: **an error message here must never contain
//! the response body or the bearer.** A collector's 4xx body routinely echoes
//! part of what was submitted, and this payload is prompt-adjacent telemetry;
//! `AGENTS.md` bans logging either. So failures report the signal, the URL and
//! the status code, which is everything an operator can act on anyway (401 =
//! token rejected, 404 = wrong base URL, 413 = batch too large).

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::redacted::Redacted;

/// The two OTLP/HTTP signal paths this drain uses. Traces are absent because
/// Copilot's file exporter writes none -- adding a `/v1/traces` POST of an
/// empty payload would be a request that can only ever fail or no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Metrics,
    Logs,
}

impl Signal {
    pub fn path(self) -> &'static str {
        match self {
            Self::Metrics => "/v1/metrics",
            Self::Logs => "/v1/logs",
        }
    }
}

impl std::fmt::Display for Signal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Metrics => formatter.write_str("metrics"),
            Self::Logs => formatter.write_str("logs"),
        }
    }
}

/// `<base>/v1/metrics`. Built by string concatenation rather than
/// [`url::Url::join`] on purpose: `join` treats the base's last path segment
/// as a file and *replaces* it, so a collector published under a path prefix
/// (`https://host/otlp`) would silently be posted to `https://host/v1/metrics`.
pub fn endpoint(base: &str, signal: Signal) -> String {
    format!("{}{}", base.trim_end_matches('/'), signal.path())
}

/// Posts one signal's payload. Anything but a 2xx is an error, so the caller
/// leaves the checkpoint where it is and the same bytes are retried next run.
pub async fn post(
    http: &reqwest::Client,
    base: &str,
    signal: Signal,
    bearer: &Redacted<String>,
    payload: &Value,
) -> Result<()> {
    let url = endpoint(base, signal);
    let response = http
        .post(&url)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", bearer.expose()))
        .json(payload)
        .send()
        .await
        .with_context(|| format!("posting {signal} to {url}"))?;

    let status = response.status();
    if !status.is_success() {
        // Body deliberately omitted -- see the module doc.
        bail!("the collector rejected the {signal} export at {url} with HTTP {status}");
    }
    Ok(())
}
