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
    /// The audiences the responding authenticator validated the token
    /// against. Per the Kubernetes API's own doc comment on
    /// `TokenReviewStatus.Audiences`, a client that sets `spec.audiences`
    /// must confirm a compatible audience is returned here; an empty list
    /// means the server was not audience-aware, which we treat as a
    /// rejection (fail closed).
    #[serde(default)]
    pub audiences: Vec<String>,
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
    /// The offending username is logged at the rejection point in `verify()`,
    /// so no payload is carried here.
    NotAllowed,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unreachable => "kube-apiserver_unreachable",
            Self::Rejected => "token_rejected",
            Self::NotAllowed => "service_account_not_allowed",
        })
    }
}

impl std::error::Error for VerifyError {}
