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

/// `billing-seats` is archived as a single JSON document under
/// `seats_archive_key` (`.json`), not as NDJSON under `archive_key` (`.ndjson`)
/// like the four day reports. `verify_archive_counts` must read the seats key
/// or the seats archive is always reported missing and the cutover can never
/// pass.
#[tokio::test]
async fn verify_archive_counts_reads_the_seats_json_key() {
    let Some(pool) = db_pool().await else { return };
    let tenant_id = format!("it-cutover-verify-seats-{}", std::process::id());
    let org = "it-cutover-verify-seats-org";
    let cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("cutover-verify-seats"),
    );

    let day = "2026-08-01";
    // A single-JSON-document seats archive (a `Vec<SeatsPage>`), as
    // `sync_seats` writes it.
    let json = concat!(
        "[{\"seats\":[{\"created_at\":\"2026-08-01T00:00:00Z\",",
        "\"last_activity_at\":null,\"last_activity_editor\":null,",
        "\"pending_cancellation_date\":null,",
        "\"assignee\":{\"id\":\"1\",\"login\":\"u1\"}}]}]",
    );
    governance_copilot::upsert_manifest(
        &pool,
        &tenant_id,
        "github_copilot",
        org,
        governance_copilot::SEATS_REPORT_TYPE,
        day,
        "ok",
        1,
    )
    .await
    .unwrap();
    let key = governance_copilot::seats_archive_key(org, day);
    cfg.archive.write(&key, json.as_bytes()).await.unwrap();

    let mismatches = verify_archive_counts(&pool, &cfg).await.unwrap();
    assert!(
        mismatches.is_empty(),
        "a complete seats archive must verify clean: {mismatches:?}"
    );
}

/// An empty day (GitHub HTTP 204) records a zero-count manifest but writes no
/// archive. `verify_archive_counts` must not report those as mismatches --
/// there is nothing archived to verify against.
#[tokio::test]
async fn verify_archive_counts_skips_empty_days_with_no_archive() {
    let Some(pool) = db_pool().await else { return };
    let tenant_id = format!("it-cutover-verify-empty-{}", std::process::id());
    let org = "it-cutover-verify-empty-org";
    let cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("cutover-verify-empty"),
    );

    let day = "2026-08-01";
    // A manifest claims 0 rows (empty day) and nothing is archived.
    governance_copilot::upsert_manifest(
        &pool,
        &tenant_id,
        "github_copilot",
        org,
        "users-1-day",
        day,
        "empty",
        0,
    )
    .await
    .unwrap();

    let mismatches = verify_archive_counts(&pool, &cfg).await.unwrap();
    assert!(
        mismatches.is_empty(),
        "an empty day with no archive must not be a mismatch: {mismatches:?}"
    );
}
