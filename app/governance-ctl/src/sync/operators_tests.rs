//! Tests for the archive-facing and status operators.

use super::operators::{SyncStatus, run_replay, run_status};
use crate::sync::test_util::{db_pool, test_config, tmp_archive_dir};

/// `run_status` end to end: `NeverSynced` for a tenant with no manifests,
/// `Synced` once one exists -- the exact distinction BLOCKER 3 required and
/// that the `(-1, -1)` sentinel erased.
#[tokio::test]
async fn run_status_distinguishes_never_synced_from_synced() {
    let Some(pool) = db_pool().await else {
        return;
    };
    let tenant_id = format!("it-ctl-status-{}", std::process::id());
    let cfg = test_config(
        tenant_id.clone(),
        "it-org".to_owned(),
        tmp_archive_dir("status"),
    );

    let status = run_status(&pool, &cfg).await.unwrap();
    assert_eq!(status, SyncStatus::NeverSynced);

    let today = chrono::Utc::now().date_naive();
    governance_copilot::upsert_manifest(
        &pool,
        &tenant_id,
        "github_copilot",
        &cfg.org,
        "organization-1-day",
        &today.format("%Y-%m-%d").to_string(),
        "ok",
        1,
    )
    .await
    .unwrap();

    let status = run_status(&pool, &cfg).await.unwrap();
    assert_eq!(
        status,
        SyncStatus::Synced {
            age_days: 0,
            unmapped_users: 0
        }
    );
}

/// `run_status` must read "never synced" as a distinct case, not "0 days old"
/// -- proved directly against the type rather than a metrics push, which
/// `metrics.rs`'s own tests cover.
#[test]
fn sync_status_never_synced_is_not_synced_zero_days_ago() {
    assert_ne!(
        SyncStatus::NeverSynced,
        SyncStatus::Synced {
            age_days: 0,
            unmapped_users: 0
        }
    );
}

/// The ADR-0014 cutover replay: with `CUTOVER_FREEZE_WRITES` on, `replay`
/// emits the archived rows through the OTLP sink (the true write path). With
/// no sink configured it must fail loudly rather than silently write nothing.
#[tokio::test]
async fn replay_with_freeze_and_no_sink_fails_loudly() {
    let Some(pool) = db_pool().await else {
        return;
    };
    let tenant_id = format!("it-ctl-replay-freeze-{}", std::process::id());
    let org = "it-org-replay-freeze";
    let mut cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("replay-freeze"),
    );
    cfg.freeze_writes = true;

    // Archive one report so replay has something to read.
    let day = "2026-08-01";
    let ndjson = concat!(
        "{\"day\":\"2026-08-01\",\"user_id\":\"1001\",\"user_login\":\"octocat\",",
        "\"total_engagements\":42,\"total_completions\":20,\"ai_credits\":2.5}\n",
    );
    let key = governance_copilot::archive_key(org, "users-1-day", day);
    cfg.archive.write(&key, ndjson.as_bytes()).await.unwrap();

    let err = run_replay(&pool, &cfg, day, day, None).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("no OTLP sink"),
        "a frozen replay with no sink must fail loudly: {err:#}"
    );
}
