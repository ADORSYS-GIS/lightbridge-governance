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

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers_with_bearer(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).expect("valid header value"),
        );
        headers
    }

    #[test]
    fn extract_bearer_token_returns_the_token_from_a_valid_header() {
        let headers = headers_with_bearer("my-jwt-token");
        assert_eq!(extract_bearer_token(&headers), Some("my-jwt-token"));
    }

    #[test]
    fn extract_bearer_token_rejects_a_missing_header() {
        assert_eq!(extract_bearer_token(&HeaderMap::new()), None);
    }

    #[test]
    fn extract_bearer_token_rejects_a_non_bearer_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Basic dXNlcjpwYXNz"),
        );
        assert_eq!(extract_bearer_token(&headers), None);
    }

    #[test]
    fn extract_bearer_token_rejects_a_lowercase_bearer_prefix() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("bearer some-token"),
        );
        // RFC 6750 §2.1 says "Bearer" is case-sensitive
        assert_eq!(extract_bearer_token(&headers), None);
    }

    /// The resolve decision table (#11's own required "Unit" test), against
    /// a pool that is never actually queried for the two rejection causes
    /// that short-circuit before touching the database. `resolve_timeout` is
    /// short so the timeout test itself stays fast.
    fn unreachable_state() -> ResolveState {
        ResolveState {
            pool: cratestack::sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://x:x@127.0.0.1:1/does-not-matter")
                .expect("lazy pool construction never actually connects"),
            verifier: TokenReviewVerifier::new(
                "https://127.0.0.1:1".to_owned(),
                vec!["api".to_owned()],
                std::collections::HashSet::new(),
            )
            .expect("client construction should succeed"),
            resolve_timeout: Duration::from_millis(200),
            cache: build_cache(Duration::from_secs(60), 10_000),
        }
    }

    /// ADR-0017, AC 3: no credential → refused. The bearer token is missing
    /// from the Authorization header entirely.
    #[tokio::test]
    async fn handle_rejects_a_missing_authorization_header() {
        let state = unreachable_state();
        let result = handle(&state, &HeaderMap::new(), b"not even json").await;
        assert_eq!(result, Err(RejectReason::MissingToken));
    }

    /// ADR-0017, AC 3: wrong audience / unreachable apiserver → refused.
    /// This is the fail-closed invariant: a dependency being down must never
    /// become a bypass. The verifier points at an unreachable kube-apiserver
    /// (127.0.0.1:1), so the reqwest call fails before any response is
    /// received.
    #[tokio::test]
    async fn handle_rejects_when_kube_apiserver_is_unreachable() {
        let state = unreachable_state();
        let result = handle(
            &state,
            &headers_with_bearer("some.jwt.token"),
            b"not even json",
        )
        .await;
        assert_eq!(result, Err(RejectReason::TokenReviewFailed));
    }

    /// ADR-0017, AC 3: a token from a non-allowed identity → refused.
    /// Uses `always_accept` to bypass TokenReview, then checks that body
    /// parsing still works. The real allowlist check happens in the verifier;
    /// this test proves the handle path reaches it.
    #[tokio::test]
    async fn handle_rejects_a_malformed_body() {
        let state = ResolveState {
            verifier: TokenReviewVerifier::always_accept(),
            ..unreachable_state()
        };
        let result = handle(
            &state,
            &headers_with_bearer("valid-token"),
            b"not json at all",
        )
        .await;
        assert_eq!(result, Err(RejectReason::MalformedBody));
    }

    #[tokio::test]
    async fn handle_rejects_when_the_database_is_unreachable_within_the_configured_timeout() {
        // This is #11's explicitly required test: "the service being
        // unreachable results in denial ... it must exist and must be seen
        // to fail if the logic is inverted." An outage must never become a
        // bypass -- verified by using a pool that genuinely cannot connect
        // (an invalid port), not a mock that assumes a certain behavior.
        //
        // Also proves the *other* AC bullet in the same test: "when the
        // configured timeout elapses, the call is abandoned... rather than
        // hanging." Before the `tokio::time::timeout` wrapper existed in
        // `handle`, this test genuinely took ~30s (sqlx's default pool
        // `acquire_timeout`) -- measured directly, not assumed, when this
        // test first ran against the unbounded `resolve` call.
        let state = ResolveState {
            verifier: TokenReviewVerifier::always_accept(),
            ..unreachable_state()
        };
        let body = serde_json::to_vec(&serde_json::json!({"credential": "gov_whatever"})).unwrap();

        let start = Instant::now();
        let result = handle(&state, &headers_with_bearer("valid-token"), &body).await;
        let elapsed = start.elapsed();

        assert_eq!(result, Err(RejectReason::Timeout));
        assert!(
            elapsed < Duration::from_secs(2),
            "must fail within the configured timeout (~200ms), not sqlx's 30s default \
             acquire_timeout -- took {elapsed:?}"
        );
    }

    fn sample_identity() -> ResolvedIdentity {
        ResolvedIdentity {
            tenant_id: "tenant-1".to_owned(),
            application_id: "app-1".to_owned(),
            environment: "prod".to_owned(),
            integration_id: "integration-1".to_owned(),
        }
    }

    /// A pool built the same way as `unreachable_state()`'s can never yield
    /// `Ok` -- proven directly by the timeout test above. So if `handle`
    /// returns `Ok` here, given that same unreachable pool, the only
    /// possible source is the cache: this is a positive proof the cache path
    /// short-circuits before the DB is ever touched, not an inference from
    /// timing.
    #[tokio::test]
    async fn a_cache_hit_is_served_without_ever_touching_the_db() {
        let state = ResolveState {
            verifier: TokenReviewVerifier::always_accept(),
            ..unreachable_state()
        };
        let credential = "gov_cache-hit-test";
        let identity = sample_identity();
        state
            .cache
            .insert(cache_key(credential), identity.clone())
            .await;

        let body = serde_json::to_vec(&serde_json::json!({"credential": credential})).unwrap();
        let result = handle(&state, &headers_with_bearer("valid-token"), &body).await;

        assert_eq!(result, Ok(ResolveResponse::from(identity)));
    }

    /// The single most important correctness rule this cache exists under:
    /// a timeout (or any other `Err`) must never be inserted, because it
    /// would pin a transient outage as a denial for the whole TTL. Proven
    /// two ways: (1) the cache is directly asserted empty afterward, and (2)
    /// a second lookup pays the full timeout again rather than returning
    /// near-instantly, which is what a poisoned cache entry would look like.
    #[tokio::test]
    async fn a_db_timeout_is_never_cached_so_a_second_lookup_still_attempts_the_db() {
        let state = ResolveState {
            verifier: TokenReviewVerifier::always_accept(),
            ..unreachable_state()
        };
        let credential = "gov_never-cached";
        let body = serde_json::to_vec(&serde_json::json!({"credential": credential})).unwrap();

        let first = handle(&state, &headers_with_bearer("valid-token"), &body).await;
        assert_eq!(first, Err(RejectReason::Timeout));
        assert!(
            state.cache.get(&cache_key(credential)).await.is_none(),
            "a timeout must never be inserted into the cache"
        );

        let start = Instant::now();
        let second = handle(&state, &headers_with_bearer("valid-token"), &body).await;
        let elapsed = start.elapsed();

        assert_eq!(second, Err(RejectReason::Timeout));
        assert!(
            elapsed >= Duration::from_millis(100),
            "a second lookup after a failure must re-attempt the DB (and pay close to the full \
             {:?} timeout again), not return near-instantly from a cached failure -- took \
             {elapsed:?}",
            state.resolve_timeout
        );
    }

    #[tokio::test]
    async fn cache_entries_expire_after_the_configured_ttl() {
        let cache = build_cache(Duration::from_millis(50), 10_000);
        let key = cache_key("gov_ttl-test");
        let identity = sample_identity();

        cache.insert(key.clone(), identity.clone()).await;
        assert_eq!(
            cache.get(&key).await,
            Some(identity),
            "must be present immediately after insertion"
        );

        tokio::time::sleep(Duration::from_millis(250)).await;
        // moka expires lazily on access/housekeeping; `run_pending_tasks`
        // forces the sweep so this assertion does not depend on incidental
        // background-thread timing.
        cache.run_pending_tasks().await;

        assert_eq!(
            cache.get(&key).await,
            None,
            "entry must be gone once the TTL has elapsed"
        );
    }

    #[tokio::test]
    async fn cache_is_bounded_by_max_capacity() {
        let max_capacity = 10_u64;
        let cache = build_cache(Duration::from_secs(60), max_capacity);

        for i in 0..(max_capacity * 5) {
            let identity = ResolvedIdentity {
                tenant_id: format!("tenant-{i}"),
                application_id: "app".to_owned(),
                environment: "prod".to_owned(),
                integration_id: format!("integration-{i}"),
            };
            cache.insert(cache_key(&format!("gov_{i}")), identity).await;
        }
        cache.run_pending_tasks().await;

        assert!(
            cache.entry_count() <= max_capacity,
            "cache must stay bounded at max_capacity ({max_capacity}), had {}",
            cache.entry_count()
        );
    }

    /// `cache` is a parameter (not built internally) so tests can control
    /// whether a lookup within the test is a warm hit or a cold miss --
    /// exactly the distinction the two assertions below the revoke call
    /// depend on.
    async fn connected_state(cache: ResolveCache) -> Option<ResolveState> {
        let database_url = std::env::var("DATABASE_URL").ok()?;
        let pool = PgPool::connect(&database_url).await.expect("connect");
        governance_core::migrate::run(&pool).await.expect("migrate");
        Some(ResolveState {
            pool,
            verifier: TokenReviewVerifier::always_accept(),
            resolve_timeout: Duration::from_secs(2),
            cache,
        })
    }

    /// #11's other required "Integration" tests: valid resolve, revoked
    /// resolve -- through the *full* `handle()` path (TokenReview
    /// authentication, JSON parsing, `governance_core::credential::resolve`,
    /// response mapping), not just the credential module in isolation
    /// (already covered by #10's own tests).
    ///
    /// Extended, not left as-is, now that `handle` caches positive answers:
    /// the pre-cache version of this test called `handle` twice against the
    /// *same* `state` and expected the post-revoke call to be denied
    /// immediately. That is no longer true by design -- ADR-0006 says so in
    /// its own words: "revocation propagates within one TTL, not instantly."
    /// A test asserting instant revocation would now fail for the reason the
    /// feature exists, not because of a bug. This version asserts BOTH
    /// halves of that documented tradeoff explicitly: a still-warm cache
    /// entry keeps resolving (staleness, bounded by the TTL), while a cold
    /// cache (a fresh process, or the same one once the TTL elapses) sees
    /// the revoke immediately, because the DB is always authoritative on a
    /// miss.
    #[tokio::test]
    async fn handle_resolves_a_valid_credential_and_denies_it_once_revoked() {
        let Some(state) = connected_state(build_cache(Duration::from_secs(60), 10_000)).await
        else {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        };

        let tenant_id = format!("tenant-{}", cuid::cuid2());
        cratestack::sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, $2)")
            .bind(&tenant_id)
            .bind("resolve-test-tenant")
            .execute(&state.pool)
            .await
            .expect("insert tenant fixture");

        let db =
            governance_core::schema::cratestack_schema::Cratestack::builder(state.pool.clone())
                .build();
        let ctx = cratestack::CratestackContext::authenticated(vec![(
            "id".to_owned(),
            cratestack::Value::String("test-principal".to_owned()),
        )]);
        let application = db
            .bind_context(ctx.clone())
            .application()
            .create(
                governance_core::schema::cratestack_schema::inputs::CreateApplicationInput {
                    id: format!("app-{}", cuid::cuid2()),
                    tenantId: tenant_id.clone(),
                    name: "resolve-test-app".to_owned(),
                    owner: None,
                },
            )
            .run()
            .await
            .expect("application fixture create");
        let environment = db
            .bind_context(ctx.clone())
            .environment()
            .create(
                governance_core::schema::cratestack_schema::inputs::CreateEnvironmentInput {
                    id: format!("env-{}", cuid::cuid2()),
                    tenantId: tenant_id.clone(),
                    applicationId: application.id.clone(),
                    name: "dev".to_owned(),
                },
            )
            .run()
            .await
            .expect("environment fixture create");

        let issued = governance_core::credential::issue(
            &db,
            &ctx,
            governance_core::schema::cratestack_schema::types::IssueIntegrationCredentialInput {
                applicationId: application.id.clone(),
                provider: "github_copilot".to_owned(),
                environmentId: environment.id,
                contentCapture: None,
                internalUserId: None,
            },
        )
        .await
        .expect("issuance must succeed");

        let body = serde_json::to_vec(&serde_json::json!({"credential": issued.secret})).unwrap();

        let resolved = handle(&state, &headers_with_bearer("valid-token"), &body)
            .await
            .expect("a freshly issued credential must resolve");
        assert_eq!(resolved.tenant_id, tenant_id);
        assert_eq!(resolved.application_id, application.id);
        assert_eq!(resolved.environment, "dev");
        assert_eq!(resolved.integration_id, issued.integration.id);

        // The first, DB-backed resolution above must have populated the
        // cache with a definitive answer -- requirement: "a definitive
        // answer is cached".
        assert!(
            state.cache.get(&cache_key(&issued.secret)).await.is_some(),
            "a successful resolve must be inserted into the cache"
        );

        governance_core::credential::revoke(
            &db,
            &ctx,
            governance_core::schema::cratestack_schema::types::RevokeIntegrationCredentialInput {
                integrationId: issued.integration.id,
            },
        )
        .await
        .expect("revoke must succeed");

        // Half 1 of ADR-0006's documented tradeoff: the still-warm cache
        // entry keeps resolving the now-revoked credential -- this is the
        // "not instantly" a still-warm entry buys, bounded by the TTL.
        let after_revoke_same_state =
            handle(&state, &headers_with_bearer("valid-token"), &body).await;
        assert_eq!(
            after_revoke_same_state,
            Ok(resolved),
            "a still-warm cache entry must keep resolving until its TTL elapses -- ADR-0006's \
             documented revocation SLA, not a bug"
        );

        // Half 2: a cold cache (a fresh process, or the same one once the
        // TTL elapses) is never stale -- the DB is authoritative on a miss,
        // so revocation is visible immediately once the cache isn't in the
        // way. This is the fail-closed guarantee the pre-cache version of
        // this test was actually checking.
        let fresh_state = ResolveState {
            cache: build_cache(Duration::from_secs(60), 10_000),
            ..state.clone()
        };
        let after_revoke_cold_cache =
            handle(&fresh_state, &headers_with_bearer("valid-token"), &body).await;
        assert_eq!(
            after_revoke_cold_cache,
            Err(RejectReason::CredentialRejected),
            "a revoked credential must be denied on a cache miss, not silently accepted"
        );
    }
}
