//! Tests for the `export-counts` operator.

use super::export_counts;
use crate::sync::test_util::{db_pool, test_config, tmp_archive_dir};

/// `export_counts` reads the expected `(day, report)` counts from
/// `ingest_manifests` and the per-table telemetry counts.
#[tokio::test]
async fn export_counts_reads_manifests_and_telemetry_counts() {
    let Some(pool) = db_pool().await else { return };
    let tenant_id = format!("it-cutover-export-{}", std::process::id());
    let org = "it-cutover-export-org";
    let cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("cutover-export"),
    );

    governance_copilot::upsert_manifest(
        &pool,
        &tenant_id,
        "github_copilot",
        org,
        "users-1-day",
        "2026-08-01",
        "ok",
        2,
    )
    .await
    .unwrap();

    let export = export_counts(&pool, &cfg).await.unwrap();
    assert_eq!(export.day_facts.len(), 1);
    assert_eq!(export.day_facts[0].day, "2026-08-01");
    assert_eq!(export.day_facts[0].report, "users-1-day");
    assert_eq!(export.day_facts[0].expected, 2);
    assert!(
        export.seat_snapshots.is_empty(),
        "no billing-seats manifest row was written"
    );
    // Telemetry tables exist (migrated) but are empty for this tenant.
    assert_eq!(export.executions, 0);
    assert_eq!(export.model_calls, 0);
    assert_eq!(export.tool_calls, 0);
}

/// `export_counts` splits `ingest_manifests` into `day_facts` (the three
/// day-fact reports) and `seat_snapshots` (`billing-seats`), and excludes
/// `user-teams-1-day` (the authz receiver refuses it, so it is not in the
/// usage store and must not be asserted -- lightbridge-authz#751).
#[tokio::test]
async fn export_counts_splits_reports_and_excludes_user_teams() {
    let Some(pool) = db_pool().await else { return };
    let tenant_id = format!("it-cutover-export-split-{}", std::process::id());
    let org = "it-cutover-export-split-org";
    let cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("cutover-export-split"),
    );

    governance_copilot::upsert_manifest(
        &pool,
        &tenant_id,
        "github_copilot",
        org,
        "organization-1-day",
        "2026-08-01",
        "ok",
        1,
    )
    .await
    .unwrap();
    governance_copilot::upsert_manifest(
        &pool,
        &tenant_id,
        "github_copilot",
        org,
        "billing-seats",
        "2026-08-01",
        "ok",
        3,
    )
    .await
    .unwrap();
    governance_copilot::upsert_manifest(
        &pool,
        &tenant_id,
        "github_copilot",
        org,
        "user-teams-1-day",
        "2026-08-01",
        "ok",
        2,
    )
    .await
    .unwrap();

    let export = export_counts(&pool, &cfg).await.unwrap();
    assert_eq!(export.day_facts.len(), 1);
    assert_eq!(export.day_facts[0].report, "organization-1-day");
    assert_eq!(export.day_facts[0].expected, 1);
    assert_eq!(export.seat_snapshots.len(), 1);
    assert_eq!(export.seat_snapshots[0].day, "2026-08-01");
    assert_eq!(export.seat_snapshots[0].expected, 3);
    assert!(
        export
            .day_facts
            .iter()
            .all(|e| e.report != "user-teams-1-day"),
        "user-teams-1-day must not be asserted (receiver refuses it)"
    );
}
