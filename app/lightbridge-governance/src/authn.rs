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
//! TokenReview is a single HTTP POST with a simple JSON body and response.
//! The `kube` crate's typed client, watch streams and runtime are unnecessary
//! overhead for this — and they pull ~200 transitive crates (`hyper`, `tower`,
//! `tonic`/`prost`) into the supply chain. `reqwest` is already in this
//! binary's dependency tree.

#[cfg(test)]
mod tests;
mod types;

use std::{collections::HashSet, time::Duration};

use reqwest::Client;
pub use types::VerifyError;
use types::{TokenReviewRequest, TokenReviewResponse, TokenReviewSpec};

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
    /// The pod's own ServiceAccount token, used to authenticate the
    /// TokenReview call to the kube-apiserver. Loaded from the in-cluster
    /// projected-token mount. Empty in local dev / tests.
    bearer_token: String,
    /// Audiences the token must carry (typically `["api"]`).
    audiences: Vec<String>,
    /// Permitted ServiceAccount identities in `namespace/name` format.
    allowed_accounts: HashSet<String>,
}

impl TokenReviewVerifier {
    /// Builds a verifier from explicit configuration.
    ///
    /// `apiserver_url` is the base URL of the kube-apiserver (e.g.
    /// `https://kubernetes.default.svc`). The `/tokenreviews` path is
    /// appended internally.
    ///
    /// The in-cluster CA bundle is loaded from
    /// `/var/run/secrets/kubernetes.io/serviceaccount/ca.crt` (the standard
    /// projected-token mount) so the kube-apiserver's self-signed certificate
    /// is trusted. If the file is absent (e.g. local dev), the client falls
    /// back to the system/webpki roots.
    ///
    /// Returns `Err` only if `reqwest::Client` construction fails — which
    /// indicates a broken TLS backend, not a caller error.
    pub fn new(
        apiserver_url: String,
        audiences: Vec<String>,
        allowed_accounts: HashSet<String>,
    ) -> Result<Self, VerifyError> {
        // Bounded timeout: must be shorter than Authorino's ext_authz timeout
        // and the `resolve_timeout` (ADR-0006). 2s is well under a typical
        // 2–5s ext_authz budget and short enough that a dead apiserver never
        // starves the Authorino step.
        let mut builder = Client::builder().timeout(Duration::from_secs(2));

        // In-cluster: trust the kube-apiserver's CA. The projected SA token
        // volume mounts the cluster CA at this path. Without it, the
        // self-signed apiserver cert fails verification and every TokenReview
        // fails closed — an outage, not a security decision.
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

        // The pod's own SA token authenticates the TokenReview call to the
        // kube-apiserver. In-cluster this is the projected-token mount; local
        // dev / tests have no such file and the token stays empty (the
        // verifier is only exercised against a real apiserver in-cluster).
        const IN_CLUSTER_TOKEN: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";
        let bearer_token = std::fs::read_to_string(IN_CLUSTER_TOKEN)
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();

        let review_url = format!("{apiserver_url}/apis/authentication.k8s.io/v1/tokenreviews");

        Ok(Self {
            client,
            review_url,
            bearer_token,
            audiences,
            allowed_accounts,
        })
    }

    /// Verifies a Bearer token via Kubernetes TokenReview.
    ///
    /// - Sends the token to the kube-apiserver with the configured audiences.
    /// - Checks `status.authenticated == true`.
    /// - Checks `status.user.username` is in the allowlist.
    /// - Every non-happy path returns `Err(VerifyError)`.
    pub async fn verify(&self, bearer_token: &str) -> Result<(), VerifyError> {
        // Test-only bypass: allows DB-backed integration tests to exercise
        // the full handle() path without a real kube-apiserver.
        #[cfg(test)]
        if self.review_url.is_empty() {
            return Ok(());
        }

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
            .bearer_auth(&self.bearer_token)
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
            return Err(VerifyError::NotAllowed(username.to_owned()));
        }

        tracing::debug!(username, "tokenreview: authenticated");
        Ok(())
    }
}
