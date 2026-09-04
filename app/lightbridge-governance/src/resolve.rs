//! `/internal/v1/resolve` for Authorino (#11, ADR-0006, ADR-0017). JSON, not
//! CBOR -- the one sanctioned exception (ADR-0009): Authorino's `metadata.http`
//! step speaks JSON and cannot be taught CBOR. Not part of the
//! cratestack-generated router -- a plain hand-written axum route, merged
//! alongside it in `main.rs`, that calls into
//! `governance_core::credential::resolve` (#10).
//!
//! Caller authentication: Kubernetes TokenReview (ADR-0017). Authorino
//! presents a projected ServiceAccount token in `Authorization: Bearer`; this
//! module extracts it and passes it to [`crate::authn::TokenReviewVerifier`],
//! which calls the kube-apiserver's TokenReview API. The shared
//! `X-Internal-Token` secret is gone (with #243 removing the ingest copy,
//! this was the last one).
//!
//! Fail-closed is the whole point (#11's own words: "the single most
//! important criterion in this story"). Every rejection -- missing token,
//! TokenReview failure, malformed body, unknown credential, revoked
//! credential, or a database error inside `resolve` -- returns the exact same
//! `401` with an empty body. There is no response that distinguishes "no such
//! tenant" from "revoked" from "you posted garbage"; that distinction exists
//! only in the `tracing` logs at the point of rejection, matching the AC.
//!
//! The cache TTL Authorino applies to this endpoint's response **is** the
//! revocation SLA -- already decided and documented in ADR-0006 and
//! `docs/runbooks/revoke-an-integration-token.md` (currently 60s). This
//! module does not redecide that; it exists underneath it.
//!
//! ## The in-process resolve cache
//!
//! ADR-0006, ADR-0007, `config/default.yaml` and the revocation runbook all
//! describe a `moka` cache in front of `governance_core::credential::resolve`
//! -- this is it. Every request to `/internal/v1/resolve` was, until now,
//! a live Postgres query; that is the exact shape of trap the platform
//! already paid for once (AGENTS.md: the Keycloak-introspection metadata
//! step, disabled 2026-07-02 because the ext_authz timeout is shorter than
//! the lookup).
//!
//! **Cache key:** a SHA-256 hex digest of the presented credential, computed
//! here rather than by widening `governance_core`'s public API --
//! `credential::hash_secret` is a private helper of that module, and adding a
//! second, cache-only export for what is otherwise the same one-line
//! computation is not worth the API surface. Never the raw secret: a heap
//! inspection or crash dump taken during the TTL must not be able to recover
//! a live credential, only a durably-unreversible digest of one.
//!
//! **What is and is not cached:** only a *definitive* answer from the
//! database -- i.e. `Ok`, a resolved identity -- is ever inserted. Every
//! `Err` path (timeout, malformed body, missing token, TokenReview failure,
//! and the `CredentialRejected` `governance_core::credential::resolve`
//! returns) is left uncached. This is deliberately more conservative than
//! "cache definitive negatives, not errors": `credential::resolve`'s own doc
//! comment says it folds a genuine database error into the *same*
//! `CredentialRejected` variant as "unknown" and "revoked" ("callers get a
//! single opaque rejection regardless of cause"), specifically so the HTTP
//! response is not a credential-enumeration oracle. That folding means this
//! module cannot tell "the credential genuinely does not exist" apart from
//! "the query errored" without widening that public type -- out of scope
//! here (see the PR description for why). Treating every `Err` as ineligible
//! to cache is therefore the only choice that satisfies the hard rule this
//! cache exists under: a transient outage must never get pinned as a denial
//! for a whole TTL. The cost is that a sustained flood of unknown/garbage
//! credentials still reaches Postgres on every request; that is strictly the
//! pre-existing behaviour, not a regression, and is bounded by the same
//! `resolve_timeout` that already protects this path.

use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use cratestack::sqlx::PgPool;
use governance_core::credential::ResolvedIdentity;
use moka::future::Cache;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::authn::TokenReviewVerifier;

// Tests live in sibling modules (kept out of this file to stay under the
// repo's 200-LoC ceiling, see `.github/actions/loc-gate`).
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_cache;
#[cfg(test)]
mod tests_db;

/// Token -> resolved identity, keyed by [`cache_key`]. Bounded (`max_capacity`)
/// and time-limited (`time_to_live`) per `config/default.yaml`'s
/// `resolveCache` defaults (60s / 10000 entries) -- moka evicts both by
/// capacity (LFU-ish, per its own docs) and by age on its own background
/// housekeeping, no manual sweep needed here.
pub type ResolveCache = Cache<String, ResolvedIdentity>;

/// Builds the cache with the given bounds. A free function (not inlined into
/// `main.rs`) so tests can build one with a short TTL without going through
/// `clap`.
pub fn build_cache(ttl: Duration, max_capacity: u64) -> ResolveCache {
    Cache::builder()
        .max_capacity(max_capacity)
        .time_to_live(ttl)
        .build()
}

/// SHA-256 hex digest of the presented credential. Deliberately independent
/// of `governance_core::credential`'s own (private) `hash_secret` -- same
/// algorithm, but this is a cache key, not the DB lookup key, and the two
/// are not required to stay byte-identical for correctness.
fn cache_key(credential: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(credential.as_bytes());
    hex::encode(hasher.finalize())
}

#[derive(Clone)]
pub struct ResolveState {
    pub pool: PgPool,
    /// Kubernetes TokenReview verifier (ADR-0017). Replaces the shared
    /// `X-Internal-Token` secret with per-caller identity: each Authorino
    /// `metadata.http` step presents a projected ServiceAccount token, and
    /// this verifier validates it via the kube-apiserver.
    pub verifier: TokenReviewVerifier,
    /// Bounds the credential lookup itself -- sqlx's own default pool
    /// `acquire_timeout` is 30s, which would hang this hot-path endpoint (and
    /// every request behind it in Authorino) for 30 real seconds under a DB
    /// outage before failing closed. Same class of trap as the disabled
    /// Keycloak-introspection step (ADR-0006): a dependency's own timeout
    /// must be shorter than the caller's, not left at a generic default.
    pub resolve_timeout: Duration,
    /// See the module doc comment. Only a definitive `Ok` answer from
    /// `governance_core::credential::resolve` is ever inserted.
    pub cache: ResolveCache,
}

#[derive(Debug, Deserialize)]
struct ResolveRequest {
    credential: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct ResolveResponse {
    #[serde(rename = "tenantId")]
    tenant_id: String,
    #[serde(rename = "applicationId")]
    application_id: String,
    environment: String,
    #[serde(rename = "integrationId")]
    integration_id: String,
}

impl From<ResolvedIdentity> for ResolveResponse {
    fn from(identity: ResolvedIdentity) -> Self {
        Self {
            tenant_id: identity.tenant_id,
            application_id: identity.application_id,
            environment: identity.environment,
            integration_id: identity.integration_id,
        }
    }
}

/// Every distinguishable-in-logs, opaque-to-the-caller rejection reason.
/// Never rendered into the HTTP response -- only into a `tracing` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RejectReason {
    /// No `Authorization: Bearer` header present.
    MissingToken,
    /// Kubernetes TokenReview failed (unreachable apiserver, not
    /// authenticated, or ServiceAccount not in allowlist). ADR-0017.
    TokenReviewFailed,
    MalformedBody,
    CredentialRejected,
    Timeout,
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::MissingToken => "missing_token",
            Self::TokenReviewFailed => "token_review_failed",
            Self::MalformedBody => "malformed_body",
            Self::CredentialRejected => "credential_rejected",
            Self::Timeout => "timeout",
        })
    }
}

/// Extracts the Bearer token from the `Authorization` header. Returns `None`
/// if the header is missing, is not valid UTF-8, or does not start with
/// `Bearer ` (case-sensitive, per RFC 6750 §2.1).
fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    let header = headers.get("authorization")?.to_str().ok()?;
    header.strip_prefix("Bearer ")
}

async fn handle(
    state: &ResolveState,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<ResolveResponse, RejectReason> {
    // ADR-0017: extract and verify the Bearer token via Kubernetes
    // TokenReview. Fail-closed on every path.
    let bearer_token = extract_bearer_token(headers).ok_or(RejectReason::MissingToken)?;
    state.verifier.verify(bearer_token).await.map_err(|error| {
        tracing::info!(%error, "resolve: token review failed");
        RejectReason::TokenReviewFailed
    })?;

    let request: ResolveRequest =
        serde_json::from_slice(body).map_err(|_| RejectReason::MalformedBody)?;

    let key = cache_key(&request.credential);
    if let Some(identity) = state.cache.get(&key).await {
        return Ok(ResolveResponse::from(identity));
    }

    let identity = tokio::time::timeout(
        state.resolve_timeout,
        governance_core::credential::resolve(&state.pool, &request.credential),
    )
    .await
    .map_err(|_| RejectReason::Timeout)?
    .map_err(|_| RejectReason::CredentialRejected)?;

    // Only reached on a genuine `Ok` from the database -- see the module doc
    // comment for why every `Err` path above returns without ever calling
    // `cache.insert`.
    state.cache.insert(key, identity.clone()).await;

    Ok(ResolveResponse::from(identity))
}

pub async fn resolve(
    State(state): State<ResolveState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let start = Instant::now();
    let outcome = handle(&state, &headers, &body).await;
    let latency_ms = start.elapsed().as_millis();

    match outcome {
        Ok(response) => {
            tracing::info!(latency_ms, "resolve: allowed");
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(reason) => {
            tracing::info!(latency_ms, %reason, "resolve: denied");
            StatusCode::UNAUTHORIZED.into_response()
        }
    }
}
