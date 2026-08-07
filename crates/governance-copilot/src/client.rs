//! Minimal HTTP client for the GitHub Copilot connector.
//!
//! Wraps a `reqwest::Client` (built once, reused). A constructor takes a
//! pre-built client so tests can substitute a mock transport without the
//! connector knowing or caring. `send_with_retry` is the one place a GitHub
//! call actually goes over the wire from `auth.rs`/`report.rs`, so bounded
//! retry with backoff lives here once rather than being reimplemented at
//! every call site.

use std::time::Duration;

use reqwest::{Client as ReqwestClient, RequestBuilder, Response, StatusCode, header::RETRY_AFTER};
use tracing::warn;

use crate::{CopilotError, Result};

/// Per-request timeout for the live client. The CronJob has
/// `activeDeadlineSeconds`; a GitHub call that stalls would otherwise hang
/// the whole daily run until the pod is killed and then hang its retry.
/// Report downloads are typically a few MB and served quickly, so 120s is
/// generous without letting a dead request wedge the run.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Default GitHub API host. Overridable only for tests (`with_api_base`) --
/// production always goes through `for_github`, which pins this.
const DEFAULT_API_BASE: &str = "https://api.github.com";

/// Attempts for a transient failure, including the first try. Bounded so a
/// cold 28-day backfill (up to 28 days * 4 reports = 112 report fetches,
/// plus the auth calls) cannot turn one flaky endpoint into a request
/// stampede against GitHub's API.
const MAX_ATTEMPTS: u32 = 3;
/// Backoff base for a transient failure that carried no `Retry-After` hint.
const RETRY_BASE_DELAY: Duration = Duration::from_millis(500);
/// Upper bound on any single sleep between attempts, including a
/// `Retry-After` value GitHub sends -- a large secondary-rate-limit hint
/// must not stall a run past the CronJob's `activeDeadlineSeconds`, and a
/// signed download URL "expires quickly" (RFC-0001) so a retry loop on the
/// download host must stay short too.
const RETRY_MAX_DELAY: Duration = Duration::from_secs(30);

/// Owned HTTP client for GitHub API + signed-report-download calls.
#[derive(Clone)]
pub struct GithubClient {
    inner: ReqwestClient,
    api_base: String,
}

impl GithubClient {
    /// Wrap a pre-built `reqwest::Client`, pointed at the real GitHub API.
    /// Tests that need a different transport (a mock server) use
    /// `with_api_base` instead.
    pub fn new(inner: ReqwestClient) -> Self {
        Self::with_api_base(inner, DEFAULT_API_BASE.to_owned())
    }

    /// As `new`, but pointed at `api_base` instead of `api.github.com`. The
    /// only production caller is `new` itself; this exists so integration
    /// tests can point the connector at a local mock server instead of
    /// reaching for `unsafe` env-var tricks or a trait-object transport.
    pub fn with_api_base(inner: ReqwestClient, api_base: String) -> Self {
        Self { inner, api_base }
    }

    /// The live client: a shared `reqwest::Client` with a per-request
    /// timeout so a stalled GitHub API call fails this run instead of
    /// hanging it.
    pub fn for_github() -> Result<Self> {
        let inner = ReqwestClient::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(CopilotError::transport)?;
        Ok(Self::new(inner))
    }

    /// The shared `reqwest::Client` (used by `AppAuth` and report fetches).
    pub fn inner(&self) -> &ReqwestClient {
        &self.inner
    }

    /// The API host this client talks to (`https://api.github.com` in
    /// production).
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// Send a request, retrying transient failures -- connection errors,
    /// timeouts, `429`, and `5xx` -- with bounded exponential backoff (or the
    /// server's own `Retry-After` hint, when present). Deterministic
    /// failures (`401`/`403`/`404`, a malformed body) are returned on the
    /// first attempt: retrying them cannot succeed and only burns rate
    /// limit (AGENTS.md).
    ///
    /// `build` is called fresh on every attempt rather than the request
    /// being cloned, so a retried call is a genuinely new request -- no
    /// consumed-body edge cases from reusing a `RequestBuilder`.
    pub(crate) async fn send_with_retry(
        &self,
        build: impl Fn() -> RequestBuilder,
    ) -> Result<Response> {
        let mut attempt = 1u32;
        loop {
            match build().send().await {
                Ok(resp) if is_retryable_status(resp.status()) && attempt < MAX_ATTEMPTS => {
                    let delay = retry_delay(attempt, Some(resp.headers()));
                    warn!(
                        status = resp.status().as_u16(),
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        "transient github response; retrying"
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Ok(resp) => return Ok(resp),
                Err(e) if is_retryable_transport(&e) && attempt < MAX_ATTEMPTS => {
                    let delay = retry_delay(attempt, None);
                    // `CopilotError::transport` strips the request URL
                    // before this is formatted -- for the signed-download
                    // call, that URL is the secret (AGENTS.md).
                    warn!(
                        error = %CopilotError::transport(e),
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        "github transport error; retrying"
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(e) => return Err(CopilotError::transport(e)),
            }
        }
    }
}

/// `5xx` and `429` are transient by construction (server overload, rate
/// limit); everything else -- including `401`/`403`/`404` -- is treated as
/// deterministic and not retried here.
fn is_retryable_status(status: StatusCode) -> bool {
    status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS
}

/// A connection that never established or a request that timed out is worth
/// retrying; a decode/builder error is a bug that retrying cannot fix.
fn is_retryable_transport(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect()
}

/// The delay before the next attempt: GitHub's `Retry-After` header when the
/// response carried one, else exponential backoff from `attempt`. Both are
/// capped at `RETRY_MAX_DELAY`.
fn retry_delay(attempt: u32, headers: Option<&reqwest::header::HeaderMap>) -> Duration {
    if let Some(secs) = headers
        .and_then(|h| h.get(RETRY_AFTER))
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
    {
        return Duration::from_secs(secs).min(RETRY_MAX_DELAY);
    }
    RETRY_BASE_DELAY
        .saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)))
        .min(RETRY_MAX_DELAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_backs_off_exponentially_without_a_header() {
        assert_eq!(retry_delay(1, None), Duration::from_millis(500));
        assert_eq!(retry_delay(2, None), Duration::from_millis(1_000));
        assert_eq!(retry_delay(3, None), Duration::from_millis(2_000));
    }

    #[test]
    fn retry_delay_is_capped() {
        // A huge attempt number must not overflow or exceed the cap.
        assert_eq!(retry_delay(20, None), RETRY_MAX_DELAY);
    }

    #[test]
    fn retry_delay_prefers_retry_after_header_over_backoff() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(RETRY_AFTER, "2".parse().unwrap());
        assert_eq!(retry_delay(1, Some(&headers)), Duration::from_secs(2));
    }

    #[test]
    fn retry_delay_caps_a_large_retry_after_header() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(RETRY_AFTER, "3600".parse().unwrap());
        assert_eq!(retry_delay(1, Some(&headers)), RETRY_MAX_DELAY);
    }

    #[test]
    fn retry_delay_ignores_an_unparseable_retry_after_header() {
        let mut headers = reqwest::header::HeaderMap::new();
        // An HTTP-date form, not the delta-seconds form we parse.
        headers.insert(
            RETRY_AFTER,
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_delay(1, Some(&headers)), Duration::from_millis(500));
    }

    #[test]
    fn is_retryable_status_covers_5xx_and_429_only() {
        assert!(is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_retryable_status(StatusCode::BAD_GATEWAY));
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        // Deterministic failures: retrying cannot succeed, only burns
        // rate limit (AGENTS.md).
        assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
        assert!(!is_retryable_status(StatusCode::FORBIDDEN));
        assert!(!is_retryable_status(StatusCode::NOT_FOUND));
        assert!(!is_retryable_status(StatusCode::OK));
    }

    #[test]
    fn default_api_base_is_the_real_github_host() {
        let client = GithubClient::new(ReqwestClient::new());
        assert_eq!(client.api_base(), "https://api.github.com");
    }

    #[test]
    fn with_api_base_overrides_the_host() {
        let client =
            GithubClient::with_api_base(ReqwestClient::new(), "http://127.0.0.1:1".to_owned());
        assert_eq!(client.api_base(), "http://127.0.0.1:1");
    }
}
