//! Minimal HTTP client for the GitHub Copilot connector.
//!
//! Wraps a `reqwest::Client` (built once, reused). A constructor takes a
//! pre-built client so tests can substitute a mock transport without the
//! connector knowing or caring.

use std::time::Duration;

use reqwest::Client as ReqwestClient;

use crate::{CopilotError, Result};

/// Per-request timeout for the live client. The CronJob has
/// `activeDeadlineSeconds`; a GitHub call that stalls would otherwise hang
/// the whole daily run until the pod is killed and then hang its retry.
/// Report downloads are typically a few MB and served quickly, so 120s is
/// generous without letting a dead request wedge the run.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Explicit `User-Agent`. reqwest's default client sends no `User-Agent`, and
/// `api.github.com` responds `403` "Request forbidden by administrative rules
/// ... make sure your request has a User-Agent header" (a plain-text,
/// multi-line body) to any request without one. The App installation and
/// credentials are irrelevant until this header is present, so the whole
/// daily run 403ed before the first real call. Verified empirically: GitHub
/// returns that 403 for `-A ""`, and a correctly-signed request with a UA
/// passes. GitHub wants a descriptive agent, not curl's.
const USER_AGENT: &str = concat!(
    "lightbridge-governance/",
    env!("CARGO_PKG_VERSION"),
    " (Copilot collector)"
);

/// Owned HTTP client for GitHub API + signed-report-download calls.
#[derive(Clone)]
pub struct GithubClient {
    inner: ReqwestClient,
}

impl GithubClient {
    /// Wrap a pre-built `reqwest::Client`. Tests inject a mock here.
    pub fn new(inner: ReqwestClient) -> Self {
        Self { inner }
    }

    /// The live client: a shared `reqwest::Client` with a per-request
    /// timeout and an explicit `User-Agent` (GitHub rejects UA-less requests
    /// with a 403) so calls fail fast and are accepted by the API.
    pub fn for_github() -> Result<Self> {
        let inner = ReqwestClient::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(CopilotError::Transport)?;
        Ok(Self::new(inner))
    }

    /// The shared `reqwest::Client` (used by `AppAuth` and report fetches).
    pub fn inner(&self) -> &ReqwestClient {
        &self.inner
    }
}
