//! Minimal HTTP client for the GitHub Copilot connector.
//!
//! Wraps a `reqwest::Client` (built once, reused). A constructor takes a
//! pre-built client so tests can substitute a mock transport without the
//! connector knowing or caring.

use reqwest::Client as ReqwestClient;

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

    /// The shared `reqwest::Client` (used by `AppAuth` and report fetches).
    pub fn inner(&self) -> &ReqwestClient {
        &self.inner
    }
}

impl Default for GithubClient {
    fn default() -> Self {
        Self::new(ReqwestClient::new())
    }
}
