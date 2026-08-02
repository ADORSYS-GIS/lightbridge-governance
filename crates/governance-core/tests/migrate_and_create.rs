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
//! 4. `applications`' hand-added `@@unique([tenantId, name])` index is real
//!    -- a second create with the same tuple is rejected. (Was
//!    `[tenantId, name, environment]` before Environment became a first-class
//!    model, #15.)
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
//! 8. `updated_at` actually advances on `UPDATE` (#21) -- the
//!    `touch_updated_at` trigger `governance_core::migrate::run` installs
//!    outside the tracked migration (see that module) fires for real.
//! 9. Integration credential issuance, resolution and revocation (#10):
//!    issuance round-trips through `resolve`; the plaintext secret never
//!    appears in a serialized `Integration` (`credentialHash` is
//!    `@server_only`); revocation makes `resolve` reject and is idempotent;
//!    an unknown credential is rejected the same opaque way.
//!
//! Requires `DATABASE_URL` (see `just up`); skips with a message otherwise so
//! it doesn't silently report green in an environment with no database.
//!
//! ## `DDL_ISOLATION` (#27)
//!
//! `migrate_is_idempotent`'s whole point is calling `governance_core::migrate::
//! run` a second time, directly, outside `MIGRATED`'s one-time guard. Since
//! #21, that function unconditionally reinstalls the `touch_updated_at`
//! triggers on every call (`AccessExclusiveLock` via `DROP TRIGGER`/`CREATE
//! TRIGGER` on every `AuditFields` table) -- previously harmless when `run`
//! was just a cheap tracked-migration check (#18), but not anymore.
//!
//! With 14 `#[tokio::test]`s genuinely concurrent in one binary, that second
//! call could interleave with another test's `INSERT` into `applications`/
//! `integrations` (which takes a `RowShareLock` on the parent table for FK
//! validation), producing a real Postgres deadlock between the DDL and the
//! DML. Confirmed, not assumed: reproduced locally (2 failures in 15 loop
//! iterations) with `log_lock_waits`/`deadlock_timeout` enabled, and the
//! captured lock graph showed exactly this -- `AccessExclusiveLock` on
//! `applications` (held by the trigger-reinstall DDL) blocking a concurrent
//! `INSERT INTO integrations` FK check, and vice versa. Not the FK-lock-
//! ordering bug #27 speculated about; `credential::issue` has no explicit
//! transaction and needed no change.
//!
//! `DDL_ISOLATION` fixes the actual race: every test holds a *read* guard for
//! its ordinary DML, and `migrate_is_idempotent` takes the *write* guard
//! before its second `run` call, so the DDL reinstall can never overlap
//! another test's insert.

use cratestack::CoolContext;
use governance_core::schema::cratestack_schema::{
    Cratestack,
    inputs::{CreateApplicationInput, UpdateApplicationInput},
    types::{IssueIntegrationCredentialInput, RevokeIntegrationCredentialInput},
};

static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
static DDL_ISOLATION: std::sync::LazyLock<std::sync::Arc<tokio::sync::RwLock<()>>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::RwLock::new(())));

#[expect(
    clippy::expect_used,
    reason = "test fixture helper, not a #[test] fn itself, so clippy's test carve-out in \
              clippy.toml doesn't cover it (see that file's note on tests/support/mod.rs); a \
              failure here means the test setup broke, not the code under test"
)]
async fn connect_and_migrate() -> Option<(
    cratestack::sqlx::PgPool,
    tokio::sync::OwnedRwLockReadGuard<()>,
)> {
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
    // Held for the rest of the calling test's lifetime -- see `DDL_ISOLATION`
    // above. `migrate_is_idempotent` drops this immediately; every other test
    // keeps it until its own scope ends.
    let guard = DDL_ISOLATION.clone().read_owned().await;
    Some((pool, guard))
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
    let Some((pool, read_guard)) = connect_and_migrate().await else {
        return;
    };
    // Drop the read guard before requesting the write guard -- holding one
    // while awaiting the other on the same lock, in the same task, deadlocks
    // at the Rust level (not just the Postgres level this whole mechanism
    // exists to avoid).
    drop(read_guard);
    let _write_guard = DDL_ISOLATION.clone().write_owned().await;

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
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
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
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
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
async fn create_application_rejects_duplicate_tenant_name() {
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
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
        "a second application with the same (tenant_id, name) must be rejected by \
         applications_tenant_id_name_key, not silently duplicated -- Environment (#15) is now \
         where per-environment distinction lives, not Application"
    );
}

#[tokio::test]
async fn ingest_manifest_rejects_duplicate_natural_key() {
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
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
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
        return;
    };
    let tenant_id = format!("tenant-{}", cuid::cuid2());
    insert_tenant(&pool, &tenant_id).await;

    let db = Cratestack::builder(pool.clone()).build();
    let ctx = authenticated_ctx();
    let application = db
        .bind_context(ctx.clone())
        .application()
        .create(CreateApplicationInput {
            id: format!("app-{}", cuid::cuid2()),
            tenantId: tenant_id,
            name: "integration-fixture-app".to_owned(),
            owner: None,
        })
        .run()
        .await
        .expect("application fixture create must succeed");
    let environment = create_environment(&db, &ctx, &application).await;

    // Integration has no `@@allow("create", ...)` policy (issuance goes
    // through the `issueIntegrationCredential` procedure, per the model's
    // own schema comment), so this goes in directly, same as the tenant
    // fixture. Real, valid `application_id`/`environment_id` isolate the
    // failure to the `tenant_id` FK specifically.
    let result = cratestack::sqlx::query(
        "INSERT INTO integrations \
         (id, tenant_id, application_id, environment_id, provider, credential_hash, status, \
          content_capture) \
         VALUES ($1, $2, $3, $4, 'github_copilot', 'hash', 'active', 'metadata_only')",
    )
    .bind(format!("integration-{}", cuid::cuid2()))
    .bind(format!("tenant-does-not-exist-{}", cuid::cuid2()))
    .bind(&application.id)
    .bind(&environment.id)
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
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
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
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
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

#[tokio::test]
async fn update_touches_updated_at() {
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
        return;
    };
    let tenant_id = format!("tenant-{}", cuid::cuid2());
    insert_tenant(&pool, &tenant_id).await;

    let db = Cratestack::builder(pool).build();
    let ctx = authenticated_ctx();
    let created = db
        .bind_context(ctx.clone())
        .application()
        .create(CreateApplicationInput {
            id: format!("app-{}", cuid::cuid2()),
            tenantId: tenant_id,
            name: "before-rename".to_owned(),
            owner: None,
        })
        .run()
        .await
        .expect("create must succeed");

    // Postgres's `now()` is the current transaction's start time, not
    // wall-clock-at-statement -- distinct enough between two separate
    // top-level statements in practice, but a short sleep removes any doubt
    // rather than relying on that.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let updated = db
        .bind_context(ctx)
        .application()
        .update(created.id.clone())
        .set(UpdateApplicationInput {
            tenantId: None,
            name: Some("after-rename".to_owned()),
            owner: None,
        })
        .run()
        .await
        .expect("update must succeed");

    assert_eq!(updated.name, "after-rename", "the update itself must apply");
    assert!(
        updated.updatedAt > created.updatedAt,
        "updated_at must advance on UPDATE via the touch_updated_at trigger -- \
         created: {:?}, updated: {:?}",
        created.updatedAt,
        updated.updatedAt
    );
    assert_eq!(
        updated.createdAt, created.createdAt,
        "createdAt must never change on UPDATE"
    );
}

#[expect(
    clippy::expect_used,
    reason = "test fixture helper, not a #[test] fn itself, so clippy's test carve-out in \
              clippy.toml doesn't cover it; a failure here means the test setup broke, not the \
              code under test"
)]
async fn create_application(
    db: &Cratestack,
    ctx: &CoolContext,
    tenant_id: &str,
) -> governance_core::schema::cratestack_schema::models::Application {
    db.bind_context(ctx.clone())
        .application()
        .create(CreateApplicationInput {
            id: format!("app-{}", cuid::cuid2()),
            tenantId: tenant_id.to_owned(),
            name: format!("credential-fixture-app-{}", cuid::cuid2()),
            owner: None,
        })
        .run()
        .await
        .expect("application fixture create must succeed")
}

#[expect(
    clippy::expect_used,
    reason = "test fixture helper, not a #[test] fn itself, so clippy's test carve-out in \
              clippy.toml doesn't cover it; a failure here means the test setup broke, not the \
              code under test"
)]
async fn create_environment(
    db: &Cratestack,
    ctx: &CoolContext,
    application: &governance_core::schema::cratestack_schema::models::Application,
) -> governance_core::schema::cratestack_schema::models::Environment {
    db.bind_context(ctx.clone())
        .environment()
        .create(
            governance_core::schema::cratestack_schema::inputs::CreateEnvironmentInput {
                id: format!("env-{}", cuid::cuid2()),
                tenantId: application.tenantId.clone(),
                applicationId: application.id.clone(),
                name: "dev".to_owned(),
            },
        )
        .run()
        .await
        .expect("environment fixture create must succeed")
}

#[tokio::test]
async fn issue_then_resolve_round_trips() {
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
        return;
    };
    let tenant_id = format!("tenant-{}", cuid::cuid2());
    insert_tenant(&pool, &tenant_id).await;

    let db = Cratestack::builder(pool.clone()).build();
    let ctx = authenticated_ctx();
    let application = create_application(&db, &ctx, &tenant_id).await;
    let environment = create_environment(&db, &ctx, &application).await;

    let issued = governance_core::credential::issue(
        &db,
        &ctx,
        IssueIntegrationCredentialInput {
            applicationId: application.id.clone(),
            provider: "github_copilot".to_owned(),
            environmentId: environment.id,
            contentCapture: None,
        },
    )
    .await
    .expect("issuance must succeed");

    assert_eq!(issued.integration.status, "active");
    assert_eq!(issued.integration.applicationId, application.id);
    assert_eq!(issued.integration.tenantId, tenant_id);

    let resolved = governance_core::credential::resolve(&pool, &issued.secret)
        .await
        .expect("a freshly issued credential must resolve");

    assert_eq!(resolved.tenant_id, tenant_id);
    assert_eq!(resolved.application_id, application.id);
    assert_eq!(resolved.environment, "dev");
    assert_eq!(resolved.integration_id, issued.integration.id);
}

#[tokio::test]
async fn issued_secret_never_appears_in_the_serialized_integration() {
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
        return;
    };
    let tenant_id = format!("tenant-{}", cuid::cuid2());
    insert_tenant(&pool, &tenant_id).await;

    let db = Cratestack::builder(pool).build();
    let ctx = authenticated_ctx();
    let application = create_application(&db, &ctx, &tenant_id).await;
    let environment = create_environment(&db, &ctx, &application).await;

    let issued = governance_core::credential::issue(
        &db,
        &ctx,
        IssueIntegrationCredentialInput {
            applicationId: application.id,
            provider: "github_copilot".to_owned(),
            environmentId: environment.id,
            contentCapture: None,
        },
    )
    .await
    .expect("issuance must succeed");

    let serialized =
        serde_json::to_string(&issued.integration).expect("Integration must serialize");

    assert!(
        !serialized.contains(&issued.secret),
        "the plaintext secret must never appear in a serialized Integration"
    );
    assert!(
        !serialized.to_lowercase().contains("credentialhash"),
        "credentialHash must be @server_only -- absent from serialized output entirely, got: \
         {serialized}"
    );
}

#[tokio::test]
async fn resolve_rejects_a_revoked_credential() {
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
        return;
    };
    let tenant_id = format!("tenant-{}", cuid::cuid2());
    insert_tenant(&pool, &tenant_id).await;

    let db = Cratestack::builder(pool.clone()).build();
    let ctx = authenticated_ctx();
    let application = create_application(&db, &ctx, &tenant_id).await;
    let environment = create_environment(&db, &ctx, &application).await;

    let issued = governance_core::credential::issue(
        &db,
        &ctx,
        IssueIntegrationCredentialInput {
            applicationId: application.id,
            provider: "github_copilot".to_owned(),
            environmentId: environment.id,
            contentCapture: None,
        },
    )
    .await
    .expect("issuance must succeed");

    governance_core::credential::resolve(&pool, &issued.secret)
        .await
        .expect("must resolve before revocation");

    let revoked = governance_core::credential::revoke(
        &db,
        &ctx,
        RevokeIntegrationCredentialInput {
            integrationId: issued.integration.id.clone(),
        },
    )
    .await
    .expect("revoke must succeed");
    assert_eq!(revoked.status, "revoked");
    assert!(revoked.revokedAt.is_some());

    let result = governance_core::credential::resolve(&pool, &issued.secret).await;
    assert!(
        result.is_err(),
        "a revoked credential must be rejected by resolve"
    );
}

#[tokio::test]
async fn revoke_is_idempotent_under_a_repeat_call() {
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
        return;
    };
    let tenant_id = format!("tenant-{}", cuid::cuid2());
    insert_tenant(&pool, &tenant_id).await;

    let db = Cratestack::builder(pool).build();
    let ctx = authenticated_ctx();
    let application = create_application(&db, &ctx, &tenant_id).await;
    let environment = create_environment(&db, &ctx, &application).await;

    let issued = governance_core::credential::issue(
        &db,
        &ctx,
        IssueIntegrationCredentialInput {
            applicationId: application.id,
            provider: "github_copilot".to_owned(),
            environmentId: environment.id,
            contentCapture: None,
        },
    )
    .await
    .expect("issuance must succeed");

    let first = governance_core::credential::revoke(
        &db,
        &ctx,
        RevokeIntegrationCredentialInput {
            integrationId: issued.integration.id.clone(),
        },
    )
    .await
    .expect("first revoke must succeed");

    let second = governance_core::credential::revoke(
        &db,
        &ctx,
        RevokeIntegrationCredentialInput {
            integrationId: issued.integration.id,
        },
    )
    .await
    .expect("revoking an already-revoked integration must be a no-op, not an error");

    assert_eq!(second.status, "revoked");
    assert_eq!(
        first.revokedAt, second.revokedAt,
        "the second revoke must not disturb the original revokedAt -- the WHERE status = \
         'active' guard should have made it a no-op"
    );
}

#[tokio::test]
async fn resolve_rejects_an_unknown_credential() {
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
        return;
    };

    let result = governance_core::credential::resolve(
        &pool,
        &format!("gov_does-not-exist-{}", cuid::cuid2()),
    )
    .await;

    assert!(
        result.is_err(),
        "an unknown credential must be rejected, not accepted"
    );
}

#[tokio::test]
async fn create_environment_under_a_nonexistent_application_is_rejected() {
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
        return;
    };
    let tenant_id = format!("tenant-{}", cuid::cuid2());
    insert_tenant(&pool, &tenant_id).await;

    // Environment has an `@@allow("create", ...)` policy, so this goes
    // through the generated CRUD, unlike Integration/IdentityMap's raw-SQL
    // fixtures elsewhere in this file.
    let db = Cratestack::builder(pool).build();
    let result = db
        .bind_context(authenticated_ctx())
        .environment()
        .create(
            governance_core::schema::cratestack_schema::inputs::CreateEnvironmentInput {
                id: format!("env-{}", cuid::cuid2()),
                tenantId: tenant_id,
                applicationId: format!("app-does-not-exist-{}", cuid::cuid2()),
                name: "dev".to_owned(),
            },
        )
        .run()
        .await;

    assert!(
        result.is_err(),
        "creating an environment under a nonexistent application must be rejected by \
         environments_application_id_fkey, not silently orphaned"
    );
}

#[tokio::test]
async fn create_environment_rejects_duplicate_natural_key() {
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
        return;
    };
    let tenant_id = format!("tenant-{}", cuid::cuid2());
    insert_tenant(&pool, &tenant_id).await;

    let db = Cratestack::builder(pool).build();
    let ctx = authenticated_ctx();
    let application = create_application(&db, &ctx, &tenant_id).await;

    let input = || governance_core::schema::cratestack_schema::inputs::CreateEnvironmentInput {
        id: format!("env-{}", cuid::cuid2()),
        tenantId: tenant_id.clone(),
        applicationId: application.id.clone(),
        name: "dev".to_owned(),
    };

    db.bind_context(ctx.clone())
        .environment()
        .create(input())
        .run()
        .await
        .expect("first create must succeed");

    let second = db
        .bind_context(ctx)
        .environment()
        .create(input())
        .run()
        .await;

    assert!(
        second.is_err(),
        "a second environment with the same (tenant_id, application_id, name) must be rejected \
         by environments_natural_key, not silently duplicated"
    );
}

#[tokio::test]
async fn issue_credential_under_a_nonexistent_environment_is_rejected() {
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
        return;
    };
    let tenant_id = format!("tenant-{}", cuid::cuid2());
    insert_tenant(&pool, &tenant_id).await;

    let db = Cratestack::builder(pool).build();
    let ctx = authenticated_ctx();
    let application = create_application(&db, &ctx, &tenant_id).await;

    let result = governance_core::credential::issue(
        &db,
        &ctx,
        IssueIntegrationCredentialInput {
            applicationId: application.id,
            provider: "github_copilot".to_owned(),
            environmentId: format!("env-does-not-exist-{}", cuid::cuid2()),
            contentCapture: None,
        },
    )
    .await;

    assert!(
        result.is_err(),
        "issuing a credential against a nonexistent environment must be rejected, not silently \
         accepted -- issue() validates the environment exists before ever reaching \
         integrations_environment_id_fkey"
    );
}

#[tokio::test]
async fn issue_credential_under_an_environment_from_a_different_application_is_rejected() {
    let Some((pool, _ddl_isolation_guard)) = connect_and_migrate().await else {
        return;
    };
    let tenant_id = format!("tenant-{}", cuid::cuid2());
    insert_tenant(&pool, &tenant_id).await;

    let db = Cratestack::builder(pool).build();
    let ctx = authenticated_ctx();
    let application_a = create_application(&db, &ctx, &tenant_id).await;
    let application_b = create_application(&db, &ctx, &tenant_id).await;
    let environment_b = create_environment(&db, &ctx, &application_b).await;

    // application_a is real, environment_b is real -- but environment_b
    // belongs to application_b. The FK alone can't catch this (it only
    // proves environment_b exists somewhere); issue()'s own cross-check must.
    let result = governance_core::credential::issue(
        &db,
        &ctx,
        IssueIntegrationCredentialInput {
            applicationId: application_a.id,
            provider: "github_copilot".to_owned(),
            environmentId: environment_b.id,
            contentCapture: None,
        },
    )
    .await;

    assert!(
        result.is_err(),
        "issuing a credential for application_a using an environment that belongs to \
         application_b must be rejected"
    );
}
