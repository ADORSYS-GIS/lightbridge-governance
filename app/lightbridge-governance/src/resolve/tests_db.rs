//! DB-backed integration tests for `/internal/v1/resolve`: a valid credential
//! resolves, and a revoked one is denied. Requires `DATABASE_URL`; skipped
//! when it is unset. Kept out of `resolve.rs` to stay under the repo's
//! 200-LoC ceiling (see `.github/actions/loc-gate`).

use std::time::Duration;

use super::{tests::headers_with_bearer, *};

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
    let Some(state) = connected_state(build_cache(Duration::from_secs(60), 10_000)).await else {
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
        governance_core::schema::cratestack_schema::Cratestack::builder(state.pool.clone()).build();
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
    let after_revoke_same_state = handle(&state, &headers_with_bearer("valid-token"), &body).await;
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
