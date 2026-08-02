//! Proves three things against a real Postgres (#18):
//!
//! 1. `governance_core::migrate::run` applies cleanly from empty and is
//!    idempotent on a second call.
//! 2. A generated `create` actually round-trips for a model whose
//!    `AuditFields` columns rely on the hand-added `DEFAULT now()` in the
//!    generated migration -- without it, this NOT NULL-violates (verified by
//!    reverting the fix locally and watching this fail before restoring it).
//! 3. `applications.tenant_id` is a real, hand-added `FOREIGN KEY` (cratestack
//!    0.5.0 does not emit one for a declared `@relation` -- see the migration
//!    comment) -- creating an `Application` under a tenant that does not
//!    exist is rejected, not silently orphaned (#9's own AC).
//!
//! Requires `DATABASE_URL` (see `just up`); skips with a message otherwise so
//! it doesn't silently report green in an environment with no database.

use cratestack::CoolContext;
use governance_core::schema::cratestack_schema::{Cratestack, inputs::CreateApplicationInput};

// `#[tokio::test]` functions in this file run concurrently against the same
// local Postgres. Applying the migration is not safe to race with itself
// (parallel `CREATE TABLE`/`CREATE TYPE` from separate connections), so the
// initial apply is deduplicated here -- `migrate_is_idempotent` still proves
// a genuine second `run` call is a no-op by calling it directly afterward.
static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

#[expect(
    clippy::expect_used,
    reason = "test fixture helper, not a #[test] fn itself, so clippy's test carve-out in \
              clippy.toml doesn't cover it (see that file's note on tests/support/mod.rs); a \
              failure here means the test setup broke, not the code under test"
)]
async fn connect_and_migrate() -> Option<cratestack::sqlx::PgPool> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return None;
        }
    };
    let pool = cratestack::sqlx::PgPool::connect(&database_url)
        .await
        .expect("connect");
    MIGRATED
        .get_or_init(|| async {
            governance_core::migrate::run(&pool)
                .await
                .expect("migrate run");
        })
        .await;
    Some(pool)
}

fn authenticated_ctx() -> CoolContext {
    CoolContext::authenticated(vec![(
        "id".to_owned(),
        cratestack::Value::String("test-principal".to_owned()),
    )])
}

/// Bypasses the generated CRUD on purpose: `Tenant` has no `@@allow("create",
/// ...)` policy (tenants are provisioned out-of-band per ADR-0001, not
/// through the public API), so a real tenant fixture for a test has to go in
/// directly.
#[expect(
    clippy::expect_used,
    reason = "test fixture helper, not a #[test] fn itself, so clippy's test carve-out in \
              clippy.toml doesn't cover it; a failure here means the test setup broke, not the \
              code under test"
)]
async fn insert_tenant(pool: &cratestack::sqlx::PgPool, id: &str) {
    cratestack::sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, $2)")
        .bind(id)
        .bind(format!("tenant for {id}"))
        .execute(pool)
        .await
        .expect("insert tenant fixture");
}

#[tokio::test]
async fn migrate_is_idempotent() {
    let Some(pool) = connect_and_migrate().await else {
        return;
    };
    let second = governance_core::migrate::run(&pool)
        .await
        .expect("second migrate run must be a no-op, not an error");
    assert!(
        second.is_empty(),
        "re-running migrate must apply nothing the second time, got {second:?}"
    );
}

#[tokio::test]
async fn create_application_round_trips_under_a_real_tenant() {
    let Some(pool) = connect_and_migrate().await else {
        return;
    };
    let tenant_id = format!("tenant-{}", cuid::cuid2());
    insert_tenant(&pool, &tenant_id).await;

    let db = Cratestack::builder(pool).build();
    let created = db
        .bind_context(authenticated_ctx())
        .application()
        .create(CreateApplicationInput {
            id: format!("app-{}", cuid::cuid2()),
            tenantId: tenant_id,
            name: "test-app".to_owned(),
            owner: None,
            environment: "dev".to_owned(),
        })
        .run()
        .await
        .expect(
            "create must succeed -- if this NOT NULL-violates on created_at/updated_at, the \
             DEFAULT now() fix in migrations/postgres/20260802000939_init/up.sql regressed",
        );

    assert_eq!(created.name, "test-app");
}

#[tokio::test]
async fn create_application_under_a_nonexistent_tenant_is_rejected() {
    let Some(pool) = connect_and_migrate().await else {
        return;
    };
    let db = Cratestack::builder(pool).build();

    let result = db
        .bind_context(authenticated_ctx())
        .application()
        .create(CreateApplicationInput {
            id: format!("app-{}", cuid::cuid2()),
            tenantId: format!("tenant-does-not-exist-{}", cuid::cuid2()),
            name: "orphan-app".to_owned(),
            owner: None,
            environment: "dev".to_owned(),
        })
        .run()
        .await;

    assert!(
        result.is_err(),
        "creating an application under a nonexistent tenant must be rejected by the \
         applications_tenant_id_fkey constraint, not silently orphaned"
    );
}
