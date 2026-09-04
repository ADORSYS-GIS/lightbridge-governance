//! Tests for the TokenReview verifier (ADR-0017), plus the test-only
//! `always_accept` constructor used by the DB-backed integration tests in
//! `resolve.rs`. Kept out of `authn.rs` to stay under the 200-LoC ceiling.

use std::{collections::HashSet, time::Duration};

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
            bearer_token: String::new(),
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
        VerifyError::NotAllowed("default/my-sa".to_owned()).to_string(),
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
