//! Wire types and errors for Kubernetes TokenReview (ADR-0017).
//!
//! Kept separate from `authn.rs` so the verifier module stays under the
//! repo's 200-LoC ceiling (see `.github/actions/loc-gate`).

use serde::{Deserialize, Serialize};

/// Kubernetes TokenReview request body — the minimal fields the API requires.
#[derive(Debug, Serialize)]
pub struct TokenReviewSpec {
    pub token: String,
    pub audiences: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TokenReviewRequest {
    #[serde(rename = "apiVersion")]
    pub api_version: &'static str,
    pub kind: &'static str,
    pub spec: TokenReviewSpec,
}

/// Kubernetes TokenReview response — only the fields we inspect.
#[derive(Debug, Deserialize)]
pub struct TokenReviewStatus {
    pub authenticated: bool,
    #[serde(default)]
    pub user: Option<TokenReviewUser>,
}

#[derive(Debug, Deserialize)]
pub struct TokenReviewUser {
    pub username: String,
}

#[derive(Debug, Deserialize)]
pub struct TokenReviewResponse {
    pub status: TokenReviewStatus,
}

/// Errors from TokenReview verification. Every variant is a rejection — there
/// is no "partial success". The `Display` implementation is for `tracing`
/// fields only; it is never rendered into the HTTP response.
#[derive(Debug)]
pub enum VerifyError {
    /// kube-apiserver is unreachable or returned a non-200 status.
    Unreachable,
    /// The token was not authenticated (expired, malformed, wrong audience).
    Rejected,
    /// The token authenticated but the ServiceAccount is not in the allowlist.
    #[allow(
        dead_code,
        reason = "consumed by Display + tracing, invisible to rustc dead-code analysis"
    )]
    NotAllowed(String),
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unreachable => "kube-apiserver_unreachable",
            Self::Rejected => "token_rejected",
            Self::NotAllowed(_) => "service_account_not_allowed",
        })
    }
}

impl std::error::Error for VerifyError {}
