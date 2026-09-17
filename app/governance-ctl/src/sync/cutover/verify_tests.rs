//! Tests for the `verify-counts` operator.

use super::verify_archive_counts;
use crate::sync::test_util::{db_pool, test_config, tmp_archive_dir};

/// A `users-1-day` NDJSON payload with `n` rows, matching the format
/// `governance-copilot`'s own `tests/store.rs` uses.
fn users_ndjson(day: &str, n: u32) -> String {
    let mut out = String::new();
    for i in 1..=n {
        out.push_str(&format!(
            "{{\"day\":\"{day}\",\"user_id\":\"{i}\",\"user_login\":\"user{i}\",\
             \"total_engagements\":1,\"total_completions\":1,\"ai_credits\":0.5}}\n"
        ));
    }
    out
}

/// `verify_archive_counts` passes when the archive matches the manifests.
#[tokio::test]
async fn verify_archive_counts_passes_when_archive_matches_manifests() {
    let Some(pool) = db_pool().await else { return };
    let tenant_id = format!("it-cutover-verify-ok-{}", std::process::id());
    let org = "it-cutover-verify-ok-org";
    let cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("cutover-verify-ok"),
    );

    let day = "2026-08-01";
    let ndjson = users_ndjson(day, 2);
    governance_copilot::upsert_manifest(
        &pool,
        &tenant_id,
        "github_copilot",
        org,
        "users-1-day",
        day,
        "ok",
        2,
    )
    .await
    .unwrap();
    let key = governance_copilot::archive_key(org, "users-1-day", day);
    cfg.archive.write(&key, ndjson.as_bytes()).await.unwrap();

    let mismatches = verify_archive_counts(&pool, &cfg).await.unwrap();
    assert!(
        mismatches.is_empty(),
        "a complete archive must verify clean: {mismatches:?}"
    );
}

/// `verify_archive_counts` reports a mismatch when the archive is missing
/// or disagrees with the manifest -- the no-loss bar that blocks the
/// cutover loudly.
#[tokio::test]
async fn verify_archive_counts_reports_a_missing_archive_as_a_mismatch() {
    let Some(pool) = db_pool().await else { return };
    let tenant_id = format!("it-cutover-verify-miss-{}", std::process::id());
    let org = "it-cutover-verify-miss-org";
    let cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("cutover-verify-miss"),
    );

    let day = "2026-08-01";
    // A manifest claims 2 rows, but nothing is archived for the day.
    governance_copilot::upsert_manifest(
        &pool,
        &tenant_id,
        "github_copilot",
        org,
        "users-1-day",
        day,
        "ok",
        2,
    )
    .await
    .unwrap();

    let mismatches = verify_archive_counts(&pool, &cfg).await.unwrap();
    assert_eq!(mismatches.len(), 1);
    assert_eq!(mismatches[0].day, day);
    assert_eq!(mismatches[0].report, "users-1-day");
    assert_eq!(mismatches[0].expected, 2);
    assert_eq!(mismatches[0].actual, 0);
}
