//! Tests for `decommission`'s shared-table handling.

use super::{COPILOT_DAY_TABLES, SHARED_TABLES, decommission};
use crate::sync::test_util::{fresh_db, replace_db, test_config, tmp_archive_dir, users_ndjson};

/// Without `--include-shared-tables`, `decommission` drops the Copilot day
/// tables but leaves the SHARED `executions`/`model_calls`/`tool_calls` tables
/// in place -- those are written by every push connector and their no-loss bar
/// is the authz-side verify-counts, so their drop must be an explicit flag.
#[tokio::test]
async fn decommission_keeps_shared_tables_without_the_flag() {
    let (pool, db_name) = fresh_db("shared").await.unwrap();
    let tenant_id = format!("it-cutover-decom-shared-{}", std::process::id());
    let org = "it-cutover-decom-shared-org";
    let cfg = test_config(
        tenant_id.clone(),
        org.to_owned(),
        tmp_archive_dir("cutover-decom-shared"),
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

    // No `--include-shared-tables`: only the Copilot day tables are dropped.
    let dropped = decommission(&pool, &cfg, true, false).await.unwrap();
    assert_eq!(dropped.len(), COPILOT_DAY_TABLES.len());

    // The Copilot day tables are gone...
    let (n,): (i64,) = cratestack::sqlx::query_as(
        "SELECT count(*) FROM information_schema.tables \
         WHERE table_name = 'copilot_org_dailys'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 0, "the Copilot day tables must be dropped");

    // ...but the shared tables survive.
    for table in SHARED_TABLES {
        let (n,): (i64,) = cratestack::sqlx::query_as(
            "SELECT count(*) FROM information_schema.tables WHERE table_name = $1",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(n, 1, "shared table {table} must survive without the flag");
    }

    // Tear down the dedicated database so it leaks nothing.
    pool.close().await;
    let admin_url = replace_db(&std::env::var("DATABASE_URL").unwrap(), "postgres");
    let admin = cratestack::sqlx::PgPool::connect(&admin_url).await.unwrap();
    let _ = cratestack::sqlx::query(&format!("DROP DATABASE IF EXISTS {db_name}"))
        .execute(&admin)
        .await;
    admin.close().await;
}
