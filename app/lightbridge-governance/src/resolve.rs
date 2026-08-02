//! `/internal/v1/resolve` for Authorino (#11, ADR-0006). JSON, not CBOR --
//! the one sanctioned exception (ADR-0009): Authorino's `metadata.http` step
//! speaks JSON and cannot be taught CBOR. Not part of the cratestack-generated
//! router -- a plain hand-written axum route, merged alongside it in
//! `main.rs`, that calls into `governance_core::credential::resolve` (#10).
//!
//! Fail-closed is the whole point (#11's own words: "the single most
//! important criterion in this story"). Every rejection -- wrong shared
//! secret, malformed body, unknown credential, revoked credential, or a
//! database error inside `resolve` -- returns the exact same `401` with an
//! empty body. There is no response that distinguishes "no such tenant" from
//! "revoked" from "you posted garbage"; that distinction exists only in the
//! `tracing` logs at the point of rejection, matching the AC.
//!
//! The cache TTL Authorino applies to this endpoint's response **is** the
//! revocation SLA -- already decided and documented in ADR-0006 and
//! `docs/runbooks/revoke-an-integration-token.md` (currently 60s). This
//! module does not redecide that; it exists underneath it.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use cratestack::sqlx::PgPool;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

#[derive(Clone)]
pub struct ResolveState {
    pub pool: PgPool,
    pub internal_token: Arc<str>,
    /// Bounds the credential lookup itself -- sqlx's own default pool
    /// `acquire_timeout` is 30s, which would hang this hot-path endpoint (and
    /// every request behind it in Authorino) for 30 real seconds under a DB
    /// outage before failing closed. Same class of trap as the disabled
    /// Keycloak-introspection step (ADR-0006): a dependency's own timeout
    /// must be shorter than the caller's, not left at a generic default.
    pub resolve_timeout: Duration,
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

impl From<governance_core::credential::ResolvedIdentity> for ResolveResponse {
    fn from(identity: governance_core::credential::ResolvedIdentity) -> Self {
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
    BadSharedSecret,
    MalformedBody,
    CredentialRejected,
    Timeout,
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::BadSharedSecret => "bad_shared_secret",
            Self::MalformedBody => "malformed_body",
            Self::CredentialRejected => "credential_rejected",
            Self::Timeout => "timeout",
        })
    }
}

fn shared_secret_is_valid(headers: &HeaderMap, expected: &str) -> bool {
    let Some(presented) = headers
        .get("x-internal-token")
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    // Constant-time: this header authenticates Authorino itself, so a timing
    // side-channel here is the same class of bug the credential module
    // already guards against for the credential comparison itself.
    bool::from(presented.as_bytes().ct_eq(expected.as_bytes()))
}

async fn handle(
    state: &ResolveState,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<ResolveResponse, RejectReason> {
    if !shared_secret_is_valid(headers, &state.internal_token) {
        return Err(RejectReason::BadSharedSecret);
    }

    let request: ResolveRequest =
        serde_json::from_slice(body).map_err(|_| RejectReason::MalformedBody)?;

    tokio::time::timeout(
        state.resolve_timeout,
        governance_core::credential::resolve(&state.pool, &request.credential),
    )
    .await
    .map_err(|_| RejectReason::Timeout)?
    .map(ResolveResponse::from)
    .map_err(|_| RejectReason::CredentialRejected)
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

    fn headers_with_token(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-internal-token", HeaderValue::from_str(token).unwrap());
        headers
    }

    #[test]
    fn shared_secret_check_accepts_the_correct_token() {
        assert!(shared_secret_is_valid(
            &headers_with_token("correct-horse-battery-staple"),
            "correct-horse-battery-staple"
        ));
    }

    #[test]
    fn shared_secret_check_rejects_a_wrong_token() {
        assert!(!shared_secret_is_valid(
            &headers_with_token("wrong"),
            "correct-horse-battery-staple"
        ));
    }

    #[test]
    fn shared_secret_check_rejects_a_missing_header() {
        assert!(!shared_secret_is_valid(
            &HeaderMap::new(),
            "correct-horse-battery-staple"
        ));
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
            internal_token: Arc::from("the-real-token"),
            resolve_timeout: Duration::from_millis(200),
        }
    }

    #[tokio::test]
    async fn handle_rejects_a_wrong_shared_secret_before_touching_the_body_or_db() {
        let state = unreachable_state();
        let result = handle(&state, &headers_with_token("wrong-token"), b"not even json").await;
        assert_eq!(result, Err(RejectReason::BadSharedSecret));
    }

    #[tokio::test]
    async fn handle_rejects_a_malformed_body() {
        let state = unreachable_state();
        let result = handle(
            &state,
            &headers_with_token("the-real-token"),
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
        let state = unreachable_state();
        let body = serde_json::to_vec(&serde_json::json!({"credential": "gov_whatever"})).unwrap();

        let start = Instant::now();
        let result = handle(&state, &headers_with_token("the-real-token"), &body).await;
        let elapsed = start.elapsed();

        assert_eq!(result, Err(RejectReason::Timeout));
        assert!(
            elapsed < Duration::from_secs(2),
            "must fail within the configured timeout (~200ms), not sqlx's 30s default \
             acquire_timeout -- took {elapsed:?}"
        );
    }

    async fn connected_state() -> Option<ResolveState> {
        let database_url = std::env::var("DATABASE_URL").ok()?;
        let pool = PgPool::connect(&database_url).await.expect("connect");
        governance_core::migrate::run(&pool).await.expect("migrate");
        Some(ResolveState {
            pool,
            internal_token: Arc::from("the-real-token"),
            resolve_timeout: Duration::from_secs(2),
        })
    }

    /// #11's other required "Integration" tests: valid resolve, revoked
    /// resolve -- through the *full* `handle()` path (shared-secret check,
    /// JSON parsing, `governance_core::credential::resolve`, response
    /// mapping), not just the credential module in isolation (already
    /// covered by #10's own tests).
    #[tokio::test]
    async fn handle_resolves_a_valid_credential_and_denies_it_once_revoked() {
        let Some(state) = connected_state().await else {
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
        let ctx = cratestack::CoolContext::authenticated(vec![(
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
                    environment: "dev".to_owned(),
                },
            )
            .run()
            .await
            .expect("application fixture create");

        let issued = governance_core::credential::issue(
            &db,
            &ctx,
            governance_core::schema::cratestack_schema::types::IssueIntegrationCredentialInput {
                applicationId: application.id.clone(),
                provider: "github_copilot".to_owned(),
                environment: "dev".to_owned(),
                contentCapture: None,
            },
        )
        .await
        .expect("issuance must succeed");

        let body = serde_json::to_vec(&serde_json::json!({"credential": issued.secret})).unwrap();

        let resolved = handle(&state, &headers_with_token("the-real-token"), &body)
            .await
            .expect("a freshly issued credential must resolve");
        assert_eq!(resolved.tenant_id, tenant_id);
        assert_eq!(resolved.application_id, application.id);
        assert_eq!(resolved.environment, "dev");
        assert_eq!(resolved.integration_id, issued.integration.id);

        governance_core::credential::revoke(
            &db,
            &ctx,
            governance_core::schema::cratestack_schema::types::RevokeIntegrationCredentialInput {
                integrationId: issued.integration.id,
            },
        )
        .await
        .expect("revoke must succeed");

        let after_revoke = handle(&state, &headers_with_token("the-real-token"), &body).await;
        assert_eq!(
            after_revoke,
            Err(RejectReason::CredentialRejected),
            "a revoked credential must be denied by resolve, not silently accepted"
        );
    }
}
