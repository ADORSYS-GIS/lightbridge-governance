//! Unit tests for the in-process `/internal/v1/resolve` cache: hits are
//! served without touching the DB, and failures are never cached. Kept out
//! of `resolve.rs` to stay under the repo's 200-LoC ceiling.

use std::time::{Duration, Instant};

use super::{
    tests::{headers_with_bearer, sample_identity, unreachable_state},
    *,
};

/// A pool built the same way as `unreachable_state()`'s can never yield
/// `Ok` -- proven directly by the timeout test in `tests.rs`. So if `handle`
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
