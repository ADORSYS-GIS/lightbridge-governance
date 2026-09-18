//! Tests for the `decommission` operator.

use super::{COPILOT_DAY_TABLES, SHARED_TABLES, decommission};
use crate::sync::test_util::{
    db_pool, fresh_db, replace_db, test_config, tmp_archive_dir, users_ndjson,
};

/// `decommission` refuses without `--confirm` -- dropping tables is
/// destructive and must be an explicit operator action.
#[tokio::test]
async fn decommission_refuses_without_confirm() {
    let Some(pool) = db_pool().await else {
        return;
    };
    let tenant_id = format!("it-cutover-decom-noconfirm-{}", std::process::id());
    let org = "it-cutover-decom-noconfirm-org";
    let cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("cutover-decom-noconfirm"),
    );

    let err = decommission(&pool, &cfg, false, false).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("--confirm"),
        "decommission without confirm must refuse: {err:#}"
    );
}

/// `decommission` blocks on a count mismatch even with `--confirm` -- the
/// no-loss bar means no table is dropped while counts are unverified.
#[tokio::test]
async fn decommission_blocks_on_a_count_mismatch() {
    let Some(pool) = db_pool().await else {
        return;
    };
    let tenant_id = format!("it-cutover-decom-block-{}", std::process::id());
    let org = "it-cutover-decom-block-org";
    let cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("cutover-decom-block"),
    );

    let day = "2026-08-01";
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

    let err = decommission(&pool, &cfg, true, false).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("blocked"),
        "decommission must block on a count mismatch: {err:#}"
    );

    // The telemetry tables must still exist -- nothing was dropped.
    let (n,): (i64,) = cratestack::sqlx::query_as(
        "SELECT count(*) FROM information_schema.tables \
         WHERE table_name = 'copilot_org_dailys'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        n, 1,
        "the telemetry tables must not be dropped on a mismatch"
    );
}

/// `decommission` drops the telemetry tables only after a clean
/// verification and an explicit `--confirm`. Runs against a dedicated
/// database because it destroys the schema.
#[tokio::test]
async fn decommission_drops_tables_after_a_clean_verify_and_confirm() {
    let (pool, db_name) = fresh_db("ok").await.unwrap();
    let tenant_id = format!("it-cutover-decom-ok-{}", std::process::id());
    let org = "it-cutover-decom-ok-org";
    let cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("cutover-decom-ok"),
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

    let dropped = decommission(&pool, &cfg, true, true).await.unwrap();
    assert_eq!(
        dropped.len(),
        COPILOT_DAY_TABLES.len() + SHARED_TABLES.len()
    );

    // The telemetry tables are gone (not dormant).
    let (n,): (i64,) = cratestack::sqlx::query_as(
        "SELECT count(*) FROM information_schema.tables \
         WHERE table_name = 'copilot_org_dailys'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        n, 0,
        "the telemetry tables must be dropped after a clean verify"
    );

    // Tear down the dedicated database so it leaks nothing.
    pool.close().await;
    let admin_url = replace_db(&std::env::var("DATABASE_URL").unwrap(), "postgres");
    let admin = cratestack::sqlx::PgPool::connect(&admin_url).await.unwrap();
    let _ = cratestack::sqlx::query(&format!("DROP DATABASE IF EXISTS {db_name}"))
        .execute(&admin)
        .await;
    admin.close().await;
}
