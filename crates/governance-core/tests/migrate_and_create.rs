//! Proves the registry schema's hand-added constraints are real, against a
//! real Postgres (#18, #16, #17). All in one file/binary on purpose: cargo
//! runs separate `tests/*.rs` binaries concurrently, and applying the
//! migration is not safe to race with itself across processes any more than
//! across threads -- see `MIGRATED` below.
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
//! 4. `applications`' hand-added `@@unique([tenantId, name, environment])`
//!    index is real -- a second create with the same tuple is rejected.
//! 5. `ingest_manifests`' hand-added `@@unique(...)` natural-key index is
//!    real -- required for #17's `ON CONFLICT DO UPDATE` design to have
//!    anything to conflict on (cratestack emits no index for a model-level
//!    `@@unique` at all; see the migration comment).
//! 6. `integrations.tenant_id` and `identity_maps.tenant_id` are real
//!    hand-added `FOREIGN KEY`s (#16) -- orphaned rows are rejected the same
//!    way `applications.tenant_id` already was.
//! 7. Reprocessing the same `ingest_manifests` natural key genuinely upserts
//!    -- `ON CONFLICT DO UPDATE` replaces the row in place rather than
//!    erroring or duplicating (#17, #9's own AC).
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

#[tokio::test]
async fn create_application_rejects_duplicate_tenant_name_environment() {
    let Some(pool) = connect_and_migrate().await else {
        return;
    };
    let tenant_id = format!("tenant-{}", cuid::cuid2());
    insert_tenant(&pool, &tenant_id).await;

    let db = Cratestack::builder(pool).build();
    let ctx = authenticated_ctx();
    let input = || CreateApplicationInput {
        id: format!("app-{}", cuid::cuid2()),
        tenantId: tenant_id.clone(),
        name: "dup-app".to_owned(),
        owner: None,
        environment: "dev".to_owned(),
    };

    db.bind_context(ctx.clone())
        .application()
        .create(input())
        .run()
        .await
        .expect("first create must succeed");

    let second = db
        .bind_context(ctx)
        .application()
        .create(input())
        .run()
        .await;

    assert!(
        second.is_err(),
        "a second application with the same (tenant_id, name, environment) must be rejected by \
         applications_tenant_id_name_environment_key, not silently duplicated"
    );
}

#[tokio::test]
async fn ingest_manifest_rejects_duplicate_natural_key() {
    let Some(pool) = connect_and_migrate().await else {
        return;
    };
    let tenant_id = format!("tenant-{}", cuid::cuid2());
    insert_tenant(&pool, &tenant_id).await;

    // IngestManifest has no `@@allow("create", ...)` policy (it's populated by
    // connectors through a procedure, not the public API yet -- see the model's
    // own schema comment), so this goes in directly, same as the tenant fixture.
    let insert = || {
        cratestack::sqlx::query(
            "INSERT INTO ingest_manifests \
             (id, tenant_id, provider, scope_id, report_day, report_type, status, \
              record_count, schema_version) \
             VALUES ($1, $2, 'github_copilot', 'org-adorsys', '2026-08-01', \
             'user_teams_1_day', 'completed', 10, 1)",
        )
        .bind(format!("manifest-{}", cuid::cuid2()))
        .bind(&tenant_id)
    };

    insert()
        .execute(&pool)
        .await
        .expect("first ingest manifest insert must succeed");

    let second = insert().execute(&pool).await;

    assert!(
        second.is_err(),
        "a second ingest manifest with the same (tenant_id, provider, scope_id, report_day, \
         report_type) must be rejected by ingest_manifests_natural_key \
         -- without it, #17's ON CONFLICT DO UPDATE reprocessing design has nothing to conflict on"
    );
}

#[tokio::test]
async fn create_integration_under_a_nonexistent_tenant_is_rejected() {
    let Some(pool) = connect_and_migrate().await else {
        return;
    };
    let tenant_id = format!("tenant-{}", cuid::cuid2());
    insert_tenant(&pool, &tenant_id).await;

    let db = Cratestack::builder(pool.clone()).build();
    let application = db
        .bind_context(authenticated_ctx())
        .application()
        .create(CreateApplicationInput {
            id: format!("app-{}", cuid::cuid2()),
            tenantId: tenant_id,
            name: "integration-fixture-app".to_owned(),
            owner: None,
            environment: "dev".to_owned(),
        })
        .run()
        .await
        .expect("application fixture create must succeed");

    // Integration has no `@@allow("create", ...)` policy (issuance goes
    // through the `issueIntegrationCredential` procedure, per the model's
    // own schema comment), so this goes in directly, same as the tenant
    // fixture. A real, valid `application_id` isolates the failure to the
    // `tenant_id` FK specifically.
    let result = cratestack::sqlx::query(
        "INSERT INTO integrations \
         (id, tenant_id, application_id, provider, environment, credential_hash, status, \
          content_capture) \
         VALUES ($1, $2, $3, 'github_copilot', 'dev', 'hash', 'active', 'metadata_only')",
    )
    .bind(format!("integration-{}", cuid::cuid2()))
    .bind(format!("tenant-does-not-exist-{}", cuid::cuid2()))
    .bind(&application.id)
    .execute(&pool)
    .await;

    assert!(
        result.is_err(),
        "creating an integration under a nonexistent tenant must be rejected by \
         integrations_tenant_id_fkey, not silently orphaned"
    );
}

#[tokio::test]
async fn create_identity_map_under_a_nonexistent_tenant_is_rejected() {
    let Some(pool) = connect_and_migrate().await else {
        return;
    };

    // IdentityMap has no `@@allow("create", ...)` policy either -- same
    // direct-insert approach as the other fixture-only models.
    let result = cratestack::sqlx::query(
        "INSERT INTO identity_maps \
         (id, tenant_id, provider, provider_user_id, mapping_source) \
         VALUES ($1, $2, 'github_copilot', 'octocat', 'verified_email')",
    )
    .bind(format!("identity-map-{}", cuid::cuid2()))
    .bind(format!("tenant-does-not-exist-{}", cuid::cuid2()))
    .execute(&pool)
    .await;

    assert!(
        result.is_err(),
        "creating an identity map under a nonexistent tenant must be rejected by \
         identity_maps_tenant_id_fkey, not silently orphaned"
    );
}

#[tokio::test]
async fn ingest_manifest_reprocessing_upserts_rather_than_duplicates() {
    let Some(pool) = connect_and_migrate().await else {
        return;
    };
    let tenant_id = format!("tenant-{}", cuid::cuid2());
    insert_tenant(&pool, &tenant_id).await;
    let manifest_id = format!("manifest-{}", cuid::cuid2());

    // First ingest: 10 records, still running.
    cratestack::sqlx::query(
        "INSERT INTO ingest_manifests \
         (id, tenant_id, provider, scope_id, report_day, report_type, status, record_count, \
          schema_version) \
         VALUES ($1, $2, 'github_copilot', 'org-adorsys', '2026-08-01', 'user_teams_1_day', \
         'running', 10, 1)",
    )
    .bind(&manifest_id)
    .bind(&tenant_id)
    .execute(&pool)
    .await
    .expect("first ingest manifest insert must succeed");

    // Reprocessing the same day: this is #9's own AC -- "a duplicate ingest of
    // the same source data... upserts rather than duplicating." A different
    // id on purpose (the connector wouldn't know the prior row's id on a
    // fresh run) -- ON CONFLICT targets the natural key, not the primary key.
    cratestack::sqlx::query(
        "INSERT INTO ingest_manifests \
         (id, tenant_id, provider, scope_id, report_day, report_type, status, record_count, \
          schema_version) \
         VALUES ($1, $2, 'github_copilot', 'org-adorsys', '2026-08-01', 'user_teams_1_day', \
         'completed', 42, 1) \
         ON CONFLICT (tenant_id, provider, scope_id, report_day, report_type) \
         DO UPDATE SET status = excluded.status, record_count = excluded.record_count, \
         updated_at = now()",
    )
    .bind(format!("manifest-{}", cuid::cuid2()))
    .bind(&tenant_id)
    .execute(&pool)
    .await
    .expect(
        "ON CONFLICT DO UPDATE must succeed -- without ingest_manifests_natural_key this has \
         nothing to conflict on",
    );

    let (row_count, id, status, record_count): (i64, String, String, i64) =
        cratestack::sqlx::query_as(
            "SELECT count(*) OVER (), id, status, record_count FROM ingest_manifests \
             WHERE tenant_id = $1 AND provider = 'github_copilot' AND scope_id = 'org-adorsys' \
             AND report_type = 'user_teams_1_day'",
        )
        .bind(&tenant_id)
        .fetch_one(&pool)
        .await
        .expect("exactly one row must exist for this natural key");

    assert_eq!(
        row_count, 1,
        "reprocessing must upsert, not duplicate the row"
    );
    assert_eq!(
        id, manifest_id,
        "the original row's id must be preserved by the upsert"
    );
    assert_eq!(
        status, "completed",
        "the upsert must have applied the new status"
    );
    assert_eq!(
        record_count, 42,
        "the upsert must have applied the new record_count"
    );
}
