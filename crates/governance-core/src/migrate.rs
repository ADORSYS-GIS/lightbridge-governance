//! Applies the migrations cratestack derives from `schema/governance.cstack`
//! (ADR-0009). Generated with `cratestack migrate diff`; there is no
//! hand-written migration anywhere in this list -- add a new `Migration` entry
//! here each time the schema changes and a new migration directory is
//! generated under `migrations/postgres/`.

use cratestack::{Migration, apply_pending, cool_error_from_sqlx, sqlx};
use sqlx::PgPool;

use crate::{Error, Result};

const INIT_UP: &str = include_str!("../migrations/postgres/20260802000939_init/up.sql");
const INIT_DOWN: &str = include_str!("../migrations/postgres/20260802000939_init/down.sql");

const INTEGRATION_CREDENTIAL_FIELDS_UP: &str =
    include_str!("../migrations/postgres/20260802051051_integration_credential_fields/up.sql");
const INTEGRATION_CREDENTIAL_FIELDS_DOWN: &str =
    include_str!("../migrations/postgres/20260802051051_integration_credential_fields/down.sql");

// Not one of the tracked `Migration`s below: `apply_pending` splits `up` on
// every literal `;` before executing each fragment as its own prepared
// statement (verified in cratestack-sqlx's migrations.rs), which corrupts any
// dollar-quoted plpgsql function body -- confirmed empirically, applying this
// as a normal migration failed with "unterminated dollar-quoted string".
// Filed upstream: https://github.com/cratestack/cratestack/issues/270
// `sqlx::raw_sql` sends the whole block as one (simple-protocol) query
// instead, so Postgres itself handles the dollar-quoting. Written
// idempotently (`CREATE OR REPLACE`, `DROP ... IF EXISTS`) since it isn't
// tracked in `cratestack_migrations` and runs on every call to `run` (#21).
const TOUCH_UPDATED_AT: &str = r#"
CREATE OR REPLACE FUNCTION touch_updated_at() RETURNS trigger AS $$
BEGIN
    NEW.updated_at = now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS applications_touch_updated_at ON applications;
CREATE TRIGGER applications_touch_updated_at
    BEFORE UPDATE ON applications
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

DROP TRIGGER IF EXISTS identity_maps_touch_updated_at ON identity_maps;
CREATE TRIGGER identity_maps_touch_updated_at
    BEFORE UPDATE ON identity_maps
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

DROP TRIGGER IF EXISTS ingest_manifests_touch_updated_at ON ingest_manifests;
CREATE TRIGGER ingest_manifests_touch_updated_at
    BEFORE UPDATE ON ingest_manifests
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

DROP TRIGGER IF EXISTS integrations_touch_updated_at ON integrations;
CREATE TRIGGER integrations_touch_updated_at
    BEFORE UPDATE ON integrations
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

DROP TRIGGER IF EXISTS tenants_touch_updated_at ON tenants;
CREATE TRIGGER tenants_touch_updated_at
    BEFORE UPDATE ON tenants
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();
"#;

fn migrations() -> Vec<Migration> {
    vec![
        Migration {
            id: "20260802000939_init".to_owned(),
            description:
                "registry: tenants, applications, integrations, identity maps, ingest manifests"
                    .to_owned(),
            up: INIT_UP.to_owned(),
            down: Some(INIT_DOWN.to_owned()),
        },
        Migration {
            id: "20260802051051_integration_credential_fields".to_owned(),
            description: "integration: credential_prefix, last_used_at, revoked_at (#10)"
                .to_owned(),
            up: INTEGRATION_CREDENTIAL_FIELDS_UP.to_owned(),
            down: Some(INTEGRATION_CREDENTIAL_FIELDS_DOWN.to_owned()),
        },
    ]
}

/// Apply every migration not yet recorded in `cratestack_migrations`, then
/// (re-)install the `touch_updated_at` triggers `apply_pending` can't carry.
/// Returns the migration ids that were newly applied (empty if already
/// current) -- the trigger step isn't reflected in the return value, since
/// it isn't a tracked migration and always runs.
pub async fn run(pool: &PgPool) -> Result<Vec<String>> {
    let applied = apply_pending(pool, &migrations())
        .await
        .map_err(Error::Storage)?;
    sqlx::raw_sql(TOUCH_UPDATED_AT)
        .execute(pool)
        .await
        .map_err(|error| Error::Storage(cool_error_from_sqlx(error)))?;
    Ok(applied)
}
