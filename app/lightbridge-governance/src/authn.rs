//! Kubernetes TokenReview-based caller authentication for `/internal/v1/resolve`
//! (ADR-0017). Replaces the shared `X-Internal-Token` secret with per-caller
//! identity: Authorino presents a projected ServiceAccount token, and this
//! module validates it via the kube-apiserver's TokenReview API.
//!
//! Fail-closed is the invariant: every non-happy path — unreachable
//! kube-apiserver, `authenticated: false`, token not in the allowlist —
//! returns `Err(VerifyError)` and the caller is refused. This sits in
//! Authorino's ext_authz hot path (ADR-0006), so a dependency's own timeout
//! must be shorter than the caller's.
//!
//! ## Why raw `reqwest`, not `kube`
//!
//! TokenReview is a single HTTP POST with a simple JSON body; the `kube`
//! crate's typed client and runtime are unnecessary overhead and pull ~200
//! transitive crates into the supply chain. `reqwest` is already a dependency.

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_token;
mod types;

use std::{collections::HashSet, path::PathBuf, time::Duration};

use reqwest::Client;
pub use types::VerifyError;
use types::{TokenReviewRequest, TokenReviewResponse, TokenReviewSpec};

/// Standard projected-token mount for the pod's own ServiceAccount token.
const IN_CLUSTER_TOKEN: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";

/// Verifies Bearer tokens via Kubernetes TokenReview (ADR-0017).
///
/// Constructed once at startup and shared across requests. The inner
/// `reqwest::Client` handles connection pooling and TLS session reuse.
#[derive(Clone)]
pub struct TokenReviewVerifier {
    client: Client,
    /// Full URL to the kube-apiserver TokenReview endpoint, e.g.
    /// `https://kubernetes.default.svc/apis/authentication.k8s.io/v1/tokenreviews`.
    review_url: String,
    /// Path to the pod's own SA token, re-read on every call so a rotated
    /// token (the kubelet rewrites the file before ~1h expiry) is picked up.
    token_path: PathBuf,
    /// Audiences the token must carry (typically `["api"]`).
    audiences: Vec<String>,
    /// Permitted ServiceAccount identities in `namespace/name` format.
    allowed_accounts: HashSet<String>,
}

impl TokenReviewVerifier {
    /// Builds a verifier from explicit configuration.
    ///
    /// `apiserver_url` is the kube-apiserver base URL; the `/tokenreviews`
    /// path is appended internally.
    ///
    /// The in-cluster CA bundle is loaded from the projected-token mount
    /// (`ca.crt`); if absent (local dev), the client uses system roots.
    ///
    /// Returns `Err` only if `reqwest::Client` construction fails.
    pub fn new(
        apiserver_url: String,
        audiences: Vec<String>,
        allowed_accounts: HashSet<String>,
    ) -> Result<Self, VerifyError> {
        // Bounded timeout: shorter than Authorino's ext_authz budget and the
        // `resolve_timeout` (ADR-0006), so a dead apiserver never starves it.
        let mut builder = Client::builder().timeout(Duration::from_secs(2));

        // In-cluster: trust the apiserver's CA from the projected-token mount.
        // Without it, every TokenReview fails closed — an outage, not a
        // security decision.
        const IN_CLUSTER_CA: &str = "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt";
        if let Ok(pem) = std::fs::read(IN_CLUSTER_CA) {
            match reqwest::Certificate::from_pem(&pem) {
                Ok(cert) => {
                    builder = builder.add_root_certificate(cert);
                }
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "tokenreview: failed to parse in-cluster CA, using system roots"
                    );
                }
            }
        } else {
            tracing::debug!("tokenreview: no in-cluster CA at {IN_CLUSTER_CA}, using system roots");
        }

        let client = builder.build().map_err(|error| {
            tracing::error!(error = %error, "tokenreview: failed to build HTTP client");
            VerifyError::Unreachable
        })?;

        let review_url = format!("{apiserver_url}/apis/authentication.k8s.io/v1/tokenreviews");

        Ok(Self {
            client,
            review_url,
            token_path: PathBuf::from(IN_CLUSTER_TOKEN),
            audiences,
            allowed_accounts,
        })
    }

    /// Verifies a Bearer token via Kubernetes TokenReview.
    ///
    /// - Sends the token with the configured audiences; checks `authenticated`,
    ///   the returned `audiences`, and the allowlist.
    /// - Every non-happy path returns `Err(VerifyError)`.
    pub async fn verify(&self, bearer_token: &str) -> Result<(), VerifyError> {
        // Test-only bypass for DB-backed integration tests.
        #[cfg(test)]
        if self.review_url.is_empty() {
            return Ok(());
        }

        // Re-read the pod's own SA token each call: the projected-token
        // volume rewrites the file before ~1h expiry, so don't cache it.
        let own_token = std::fs::read_to_string(&self.token_path)
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();

        let request = TokenReviewRequest {
            api_version: "authentication.k8s.io/v1",
            kind: "TokenReview",
            spec: TokenReviewSpec {
                token: bearer_token.to_owned(),
                audiences: self.audiences.clone(),
            },
        };

        let response = self
            .client
            .post(&self.review_url)
            .bearer_auth(&own_token)
            .json(&request)
            .send()
            .await
            .map_err(|error| {
                tracing::warn!(error = %error, "tokenreview: kube-apiserver unreachable, failing closed");
                VerifyError::Unreachable
            })?;

        if !response.status().is_success() {
            tracing::warn!(
                status = %response.status(),
                "tokenreview: kube-apiserver returned non-200, failing closed"
            );
            return Err(VerifyError::Unreachable);
        }

        let review: TokenReviewResponse = response.json().await.map_err(|error| {
            tracing::warn!(error = %error, "tokenreview: failed to parse response, failing closed");
            VerifyError::Unreachable
        })?;

        if !review.status.authenticated {
            tracing::info!("tokenreview: token not authenticated");
            return Err(VerifyError::Rejected);
        }

        // The apiserver must confirm it validated the token against one of
        // our requested audiences; an empty `status.audiences` (not
        // audience-aware) is a rejection, not "any audience is fine".
        if !review
            .status
            .audiences
            .iter()
            .any(|a| self.audiences.contains(a))
        {
            tracing::info!("tokenreview: token not validated for a requested audience");
            return Err(VerifyError::Rejected);
        }

        let username = review
            .status
            .user
            .as_ref()
            .map_or("", |u| u.username.as_str());

        // Kubernetes reports the caller as `system:serviceaccount:<ns>:<name>`.
        // The allowlist is configured in `<ns>/<name>` format (ADR-0017), so
        // normalize before comparing. A username that doesn't match the
        // serviceaccount shape is treated as not-allowed (fail closed).
        let normalized = username
            .strip_prefix("system:serviceaccount:")
            .map_or_else(|| username.to_owned(), |s| s.replace(':', "/"));

        if !self.allowed_accounts.contains(&normalized) {
            tracing::info!(username, "tokenreview: service account not in allowlist");
            return Err(VerifyError::NotAllowed);
        }

        tracing::debug!(username, "tokenreview: authenticated");
        Ok(())
    }
}
