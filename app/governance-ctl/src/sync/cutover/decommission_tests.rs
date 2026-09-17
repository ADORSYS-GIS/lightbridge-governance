//! Tests for the `decommission` operator.

use super::{TELEMETRY_TABLES, decommission};
use crate::sync::test_util::{db_pool, test_config, tmp_archive_dir};

/// A dedicated, freshly-migrated database for the destructive
/// `decommission` test. `decommission` drops the telemetry tables, which
/// would destroy the shared test schema every other test depends on, so it
/// must run against its own database. Returns the pool and the database
/// name; the caller drops the database (after closing the pool) when done.
///
/// Fails loudly (rather than silently skipping) if the database cannot be
/// created -- a destructive test that silently no-ops would be a green
/// job that ran nothing.
async fn fresh_db(label: &str) -> anyhow::Result<(cratestack::sqlx::PgPool, String)> {
    let url = std::env::var("DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("DATABASE_URL must be set to run the decommission test"))?;
    // Point at the server's `postgres` maintenance database to run DDL.
    let admin_url = replace_db(&url, "postgres");
    let db_name = format!("lb_decom_{label}_{}", std::process::id());
    let admin = cratestack::sqlx::PgPool::connect(&admin_url)
        .await
        .map_err(|e| anyhow::anyhow!("connecting to postgres maintenance db: {e}"))?;
    let _ = cratestack::sqlx::query(&format!("DROP DATABASE IF EXISTS {db_name}"))
        .execute(&admin)
        .await;
    cratestack::sqlx::query(&format!("CREATE DATABASE {db_name}"))
        .execute(&admin)
        .await
        .map_err(|e| anyhow::anyhow!("creating decommission test db: {e}"))?;
    admin.close().await;
    let pool = cratestack::sqlx::PgPool::connect(&replace_db(&url, &db_name))
        .await
        .map_err(|e| anyhow::anyhow!("connecting to decommission test db: {e}"))?;
    governance_core::migrate::run(&pool)
        .await
        .map_err(|e| anyhow::anyhow!("migrating decommission test db: {e}"))?;
    Ok((pool, db_name))
}

/// Swap the database name in a `postgres://` URL.
fn replace_db(url: &str, db: &str) -> String {
    // postgres://user:pass@host:port/db -> postgres://user:pass@host:port/<db>
    let (head, _tail) = url.rsplit_once('/').unwrap_or((url, ""));
    format!("{head}/{db}")
}

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

    let err = decommission(&pool, &cfg, false).await.unwrap_err();
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

    let err = decommission(&pool, &cfg, true).await.unwrap_err();
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

    let dropped = decommission(&pool, &cfg, true).await.unwrap();
    assert_eq!(dropped.len(), TELEMETRY_TABLES.len());

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
