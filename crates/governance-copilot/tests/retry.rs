//! Proves `GithubClient::send_with_retry` (client.rs) does what "ALSO FIX"
//! in the pre-go-live review demanded: a transient failure (5xx, 429) is
//! retried with bounded backoff and eventually succeeds; a deterministic
//! failure (401/403/404) is returned on the first attempt, never retried;
//! and retries are bounded so a run cannot stampede GitHub.
//!
//! Exercised through the real public entry points (`fetch_report`,
//! `AppAuth::token_for_org`) against a local mock server, not by calling the
//! private retry helper directly -- this is what actually proves the retry
//! is wired into the production code paths, not just that the helper works
//! in isolation.

mod support;

use std::time::Instant;

use governance_copilot::{AppAuth, GithubClient, RawSecret};
use reqwest::Client as ReqwestClient;
use support::{
    mock_github::{MockGithub, MockGithubConfig, RouteBehavior},
    test_app_key::TEST_APP_PRIVATE_KEY_PEM,
};

fn client_for(mock: &MockGithub) -> GithubClient {
    GithubClient::with_api_base(ReqwestClient::new(), mock.base_url.clone())
}

#[tokio::test]
async fn fetch_report_retries_a_transient_5xx_then_succeeds() {
    let mock = MockGithub::start(MockGithubConfig {
        report: RouteBehavior::fails_then_succeeds(1, 500),
        ..Default::default()
    })
    .await
    .unwrap();
    let client = client_for(&mock);
    let token = RawSecret::new("test-token".to_owned());

    let report = client
        .fetch_report("test-org", "organization-1-day", "2026-08-01", &token)
        .await
        .unwrap();

    assert!(!report.empty);
    assert_eq!(mock.report_call_count().unwrap(), 2, "one retry expected");
    // The download route was reached too -- proves the retried call still
    // completed the full fetch, not just the first leg.
    assert_eq!(mock.download_call_count().unwrap(), 1);
}

#[tokio::test]
async fn fetch_report_does_not_retry_a_404() {
    let mock = MockGithub::start(MockGithubConfig {
        report: RouteBehavior::always_fails(404),
        ..Default::default()
    })
    .await
    .unwrap();
    let client = client_for(&mock);
    let token = RawSecret::new("test-token".to_owned());

    let err = client
        .fetch_report("test-org", "organization-1-day", "2026-08-01", &token)
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("404"),
        "expected a 404 error, got: {err}"
    );
    assert_eq!(
        mock.report_call_count().unwrap(),
        1,
        "a deterministic 404 must not be retried"
    );
}

#[tokio::test]
async fn fetch_report_gives_up_after_bounded_retries() {
    let mock = MockGithub::start(MockGithubConfig {
        report: RouteBehavior::always_fails(503),
        ..Default::default()
    })
    .await
    .unwrap();
    let client = client_for(&mock);
    let token = RawSecret::new("test-token".to_owned());

    let err = client
        .fetch_report("test-org", "organization-1-day", "2026-08-01", &token)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("503"));
    // MAX_ATTEMPTS is 3 (client.rs): the run must not retry forever against
    // a wedged endpoint.
    assert_eq!(mock.report_call_count().unwrap(), 3);
}

#[tokio::test]
async fn fetch_report_respects_the_retry_after_header() {
    let mock = MockGithub::start(MockGithubConfig {
        report: RouteBehavior::fails_with_retry_after(1, 429, 3),
        ..Default::default()
    })
    .await
    .unwrap();
    let client = client_for(&mock);
    let token = RawSecret::new("test-token".to_owned());

    let start = Instant::now();
    client
        .fetch_report("test-org", "organization-1-day", "2026-08-01", &token)
        .await
        .unwrap();
    let elapsed = start.elapsed();

    // The header said 3s; the default first-attempt backoff (no header)
    // would be 500ms. A pass here can only happen if the header was read,
    // not the exponential-backoff default.
    assert!(
        elapsed >= std::time::Duration::from_secs(3),
        "expected the Retry-After header to be honored, waited only {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "retry took implausibly long: {elapsed:?}"
    );
}

#[tokio::test]
async fn token_for_org_retries_a_transient_5xx_on_installation_lookup() {
    let mock = MockGithub::start(MockGithubConfig {
        installations: RouteBehavior::fails_then_succeeds(1, 502),
        installed_org: "test-org".to_owned(),
        ..Default::default()
    })
    .await
    .unwrap();
    let client = client_for(&mock);
    let auth = AppAuth::new(
        "123456".to_owned(),
        RawSecret::new(TEST_APP_PRIVATE_KEY_PEM.to_owned()),
        &client,
    );

    let token = auth.token_for_org("test-org").await.unwrap();
    assert!(!token.as_ref().is_empty());
    assert_eq!(mock.installations_call_count().unwrap(), 2);
}

#[tokio::test]
async fn token_for_org_does_not_retry_a_deterministic_403() {
    let mock = MockGithub::start(MockGithubConfig {
        access_token: RouteBehavior::always_fails(403),
        installed_org: "test-org".to_owned(),
        ..Default::default()
    })
    .await
    .unwrap();
    let client = client_for(&mock);
    let auth = AppAuth::new(
        "123456".to_owned(),
        RawSecret::new(TEST_APP_PRIVATE_KEY_PEM.to_owned()),
        &client,
    );

    let err = auth.token_for_org("test-org").await.unwrap_err();
    assert!(err.to_string().contains("403"));
    assert_eq!(
        mock.access_token_call_count().unwrap(),
        1,
        "a deterministic 403 must not be retried"
    );
}
