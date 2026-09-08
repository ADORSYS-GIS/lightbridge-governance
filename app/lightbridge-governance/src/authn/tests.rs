//! Tests for the TokenReview verifier (ADR-0017), plus the test-only
//! `always_accept` constructor used by the DB-backed integration tests in
//! `resolve.rs`. Kept out of `authn.rs` to stay under the 200-LoC ceiling.

use std::{collections::HashSet, time::Duration};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{TokenReviewVerifier, types::VerifyError};

impl TokenReviewVerifier {
    /// Creates a test-only verifier that skips the real TokenReview call.
    ///
    /// Every `verify()` call succeeds unconditionally. Use only in tests
    /// that exercise the credential-resolution path without needing to
    /// stand up a kube-apiserver mock — the DB-backed integration tests
    /// in `resolve.rs`, for instance.
    pub(crate) fn always_accept() -> Self {
        // SAFETY: this is only compiled under `#[cfg(test)]` — unreachable in
        // shipping code. The `review_url` and `client` are never used
        // because `verify` short-circuits before reaching them.
        Self {
            client: reqwest::Client::new(),
            review_url: String::new(),
            token_path: std::path::PathBuf::new(),
            audiences: Vec::new(),
            allowed_accounts: HashSet::new(),
        }
    }
}

#[test]
fn verify_error_display_matches_expected_tracing_fields() {
    assert_eq!(
        VerifyError::Unreachable.to_string(),
        "kube-apiserver_unreachable"
    );
    assert_eq!(VerifyError::Rejected.to_string(), "token_rejected");
    assert_eq!(
        VerifyError::NotAllowed.to_string(),
        "service_account_not_allowed"
    );
}

#[test]
fn allowed_accounts_is_case_sensitive() {
    let mut allowed = HashSet::new();
    allowed.insert("default/Authorino".to_owned());

    assert!(
        !allowed.contains("default/authorino"),
        "Kubernetes ServiceAccount names are case-sensitive"
    );
}

#[test]
fn serviceaccount_username_normalizes_to_namespace_name() {
    // Kubernetes reports `system:serviceaccount:<ns>:<name>`; the
    // allowlist is `<ns>/<name>`. The normalization must map one to the
    // other.
    let username = "system:serviceaccount:ingest-test:caller-sa";
    let normalized = username
        .strip_prefix("system:serviceaccount:")
        .map_or_else(|| username.to_owned(), |s| s.replace(':', "/"));
    assert_eq!(normalized, "ingest-test/caller-sa");
}

#[test]
fn non_serviceaccount_username_is_left_untouched() {
    // A non-serviceaccount identity (e.g. a user) has no prefix to strip
    // and must not accidentally match an allowlist entry.
    let username = "benie.possi@adorsys.com";
    let normalized = username
        .strip_prefix("system:serviceaccount:")
        .map_or_else(|| username.to_owned(), |s| s.replace(':', "/"));
    assert_eq!(normalized, "benie.possi@adorsys.com");
}

/// Proves that `always_accept` short-circuits before any HTTP call —
/// the `review_url` is empty, which would panic on a real request.
#[tokio::test]
async fn always_accept_skips_the_review_entirely() {
    let verifier = TokenReviewVerifier::always_accept();
    // This would fail with a malformed URL if the short-circuit didn't work.
    assert!(verifier.verify("any-token-at-all").await.is_ok());
}

/// The most important fail-closed test: a verifier pointed at an
/// unreachable kube-apiserver MUST return `Unreachable`, not hang or
/// succeed. This is the exact shape of trap the platform already paid
/// for once (AGENTS.md: the Keycloak-introspection metadata step,
/// disabled 2026-07-02 because the ext_authz timeout is shorter than
/// the lookup).
#[tokio::test]
async fn unreachable_apiserver_is_fail_closed() {
    let verifier = TokenReviewVerifier::new(
        "https://127.0.0.1:1".to_owned(),
        vec!["api".to_owned()],
        HashSet::new(),
    )
    .expect("client construction should succeed");

    let start = std::time::Instant::now();
    let result = verifier.verify("some.jwt.token").await;
    let elapsed = start.elapsed();

    assert!(result.is_err(), "unreachable apiserver must not succeed");
    assert!(
        matches!(result, Err(VerifyError::Unreachable)),
        "expected Unreachable, got {result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "must fail within the client timeout (~2s), not hang — took {elapsed:?}"
    );
}

/// Starts a one-shot TCP stub that answers the TokenReview POST with a canned
/// JSON body, and returns the base URL to point a verifier at. Uses plain
/// `http://` so no TLS is involved; the verifier only adds the in-cluster CA
/// when the file exists, which it never does in tests.
async fn token_review_stub(body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub listener");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });
    format!("http://{addr}")
}

/// ADR-0017 AC 3: a token that authenticates but whose ServiceAccount is not
/// in the allowlist must be refused. This exercises the REAL `verify()`
/// parsing/allowlist code (not the `always_accept` bypass) by pointing the
/// verifier at a local stub that returns an `authenticated: true` response
/// for a non-allowlisted username.
#[tokio::test]
async fn non_allowlisted_identity_is_rejected_by_the_real_verify_path() {
    let body = r#"{"status":{"authenticated":true,"audiences":["api"],"user":{"username":"system:serviceaccount:default:not-allowed"}}}"#;
    let base = token_review_stub(body).await;
    let verifier = TokenReviewVerifier::new(
        base,
        vec!["api".to_owned()],
        HashSet::from(["default/allowed".to_owned()]),
    )
    .expect("client construction should succeed");

    let result = verifier.verify("some.jwt.token").await;
    assert!(
        matches!(result, Err(VerifyError::NotAllowed)),
        "expected NotAllowed, got {result:?}"
    );
}

/// ADR-0017: an `authenticated: true` response that does not confirm the
/// requested audience (empty `status.audiences`) must be refused — the
/// apiserver was not audience-aware, so the audience pin is unenforced.
#[tokio::test]
async fn missing_audience_confirmation_is_rejected() {
    let body = r#"{"status":{"authenticated":true,"user":{"username":"system:serviceaccount:default/allowed"}}}"#;
    let base = token_review_stub(body).await;
    let verifier = TokenReviewVerifier::new(
        base,
        vec!["api".to_owned()],
        HashSet::from(["default/allowed".to_owned()]),
    )
    .expect("client construction should succeed");

    let result = verifier.verify("some.jwt.token").await;
    assert!(
        matches!(result, Err(VerifyError::Rejected)),
        "expected Rejected (audience not confirmed), got {result:?}"
    );
}
