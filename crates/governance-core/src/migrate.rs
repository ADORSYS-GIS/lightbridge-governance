//! Applies the migrations cratestack derives from `schema/governance.cstack`
//! (ADR-0009). Generated with `cratestack migrate diff`; there is no
//! hand-written migration anywhere in this list -- add a new `Migration` entry
//! here each time the schema changes and a new migration directory is
//! generated under `migrations/postgres/`.

use cratestack::{Migration, apply_pending, cratestack_error_from_sqlx, sqlx};
use sqlx::PgPool;

use crate::{Error, Result};

const INIT_UP: &str = include_str!("../migrations/postgres/20260802000939_init/up.sql");
const INIT_DOWN: &str = include_str!("../migrations/postgres/20260802000939_init/down.sql");

const INTEGRATION_CREDENTIAL_FIELDS_UP: &str =
    include_str!("../migrations/postgres/20260802051051_integration_credential_fields/up.sql");
const INTEGRATION_CREDENTIAL_FIELDS_DOWN: &str =
    include_str!("../migrations/postgres/20260802051051_integration_credential_fields/down.sql");

const ENVIRONMENT_MODEL_UP: &str =
    include_str!("../migrations/postgres/20260802142154_environment_model/up.sql");
const ENVIRONMENT_MODEL_DOWN: &str =
    include_str!("../migrations/postgres/20260802142154_environment_model/down.sql");

const TELEMETRY_MODELS_UP: &str =
    include_str!("../migrations/postgres/20260803000001_telemetry_models/up.sql");
const TELEMETRY_MODELS_DOWN: &str =
    include_str!("../migrations/postgres/20260803000001_telemetry_models/down.sql");
const COPILOT_USAGE_MODELS_UP: &str =
    include_str!("../migrations/postgres/20260804114257_copilot_usage_models/up.sql");
const COPILOT_USAGE_MODELS_DOWN: &str =
    include_str!("../migrations/postgres/20260804114257_copilot_usage_models/down.sql");

// Not one of the tracked `Migration`s below: `apply_pending` splits `up` on
// every literal `;` before executing each fragment as its own prepared
// statement (verified in cratestack-sqlx's migrations.rs), which corrupts any
// dollar-quoted plpgsql function body -- confirmed empirically, applying this
// as a normal migration failed with "unterminated dollar-quoted string".
// Filed upstream: https://github.com/cratestack/cratestack/issues/270
// `sqlx::raw_sql` sends the whole block as one (simple-protocol) query
// instead, so Postgres itself handles the dollar-quoting. Written
// idempotently (`CREATE OR REPLACE`, `DROP ... IF EXISTS`) since it isn't
// tracked in `cratestack_migrations`. It runs only when a trigger is
// missing (see `TRIGGERS_INSTALLED`): `DROP TRIGGER`/`CREATE TRIGGER` take
// AccessExclusiveLock on the table, and re-running them on every call raced
// another process's inserts into a 40P01 deadlock (CI, two test binaries
// migrating one shared database at once; same race in production when two
// pods boot together).
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

DROP TRIGGER IF EXISTS environments_touch_updated_at ON environments;
CREATE TRIGGER environments_touch_updated_at
    BEFORE UPDATE ON environments
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

DROP TRIGGER IF EXISTS executions_touch_updated_at ON executions;
CREATE TRIGGER executions_touch_updated_at
    BEFORE UPDATE ON executions
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

DROP TRIGGER IF EXISTS model_calls_touch_updated_at ON model_calls;
CREATE TRIGGER model_calls_touch_updated_at
    BEFORE UPDATE ON model_calls
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

DROP TRIGGER IF EXISTS tool_calls_touch_updated_at ON tool_calls;
CREATE TRIGGER tool_calls_touch_updated_at
    BEFORE UPDATE ON tool_calls
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

DROP TRIGGER IF EXISTS model_pricing_touch_updated_at ON model_pricing;
CREATE TRIGGER model_pricing_touch_updated_at
    BEFORE UPDATE ON model_pricing
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

DROP TRIGGER IF EXISTS copilot_org_dailys_touch_updated_at ON copilot_org_dailys;
CREATE TRIGGER copilot_org_dailys_touch_updated_at
    BEFORE UPDATE ON copilot_org_dailys
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

DROP TRIGGER IF EXISTS copilot_user_dailys_touch_updated_at ON copilot_user_dailys;
CREATE TRIGGER copilot_user_dailys_touch_updated_at
    BEFORE UPDATE ON copilot_user_dailys
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

DROP TRIGGER IF EXISTS copilot_repo_dailys_touch_updated_at ON copilot_repo_dailys;
CREATE TRIGGER copilot_repo_dailys_touch_updated_at
    BEFORE UPDATE ON copilot_repo_dailys
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

DROP TRIGGER IF EXISTS copilot_user_teams_touch_updated_at ON copilot_user_teams;
CREATE TRIGGER copilot_user_teams_touch_updated_at
    BEFORE UPDATE ON copilot_user_teams
    FOR EACH ROW EXECUTE FUNCTION touch_updated_at();

DROP TRIGGER IF EXISTS copilot_seat_snapshots_touch_updated_at ON copilot_seat_snapshots;
CREATE TRIGGER copilot_seat_snapshots_touch_updated_at
    BEFORE UPDATE ON copilot_seat_snapshots
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
        Migration {
            id: "20260802142154_environment_model".to_owned(),
            description: "environment: first-class model, replacing the bare environment \
                           string on applications/integrations (#15)"
                .to_owned(),
            up: ENVIRONMENT_MODEL_UP.to_owned(),
            down: Some(ENVIRONMENT_MODEL_DOWN.to_owned()),
        },
        Migration {
            id: "20260803000001_telemetry_models".to_owned(),
            description: "telemetry: executions, model_calls, tool_calls, model_pricing (#30)"
                .to_owned(),
            up: TELEMETRY_MODELS_UP.to_owned(),
            down: Some(TELEMETRY_MODELS_DOWN.to_owned()),
        },
        Migration {
            id: "20260804114257_copilot_usage_models".to_owned(),
            description: "copilot: org/user/repo daily, user-teams, and seat snapshot \
                           normalized report tables (RFC-0001, #12)"
                .to_owned(),
            up: COPILOT_USAGE_MODELS_UP.to_owned(),
            down: Some(COPILOT_USAGE_MODELS_DOWN.to_owned()),
        },
    ]
}

/// Serialize migration runs across processes.
///
/// `run` executes DDL whenever it finds unrecorded migrations or missing
/// triggers, so two concurrent callers could interleave CREATE TABLE /
/// CREATE TRIGGER with another process's inserts and deadlock (40P01). Seen
/// in CI, where the governance-core and governance-copilot test binaries
/// migrate one shared database at once; the same race exists in production
/// when two pods boot together against a fresh database. Arbitrary
/// per-workspace key.
const MIGRATE_LOCK_KEY: i64 = 0x4C42_4D49_4700_0001;

/// Count of `*_touch_updated_at` triggers `TOUCH_UPDATED_AT` installs; keep
/// in step with the trigger statements in that block.
const EXPECTED_TOUCH_TRIGGERS: i64 = 15;

/// True when every `touch_updated_at` trigger exists. One cheap catalog
/// read that keeps the steady-state `run` free of table-level DDL.
const TRIGGERS_INSTALLED: &str =
    "SELECT count(*) FROM pg_trigger WHERE tgname LIKE '%_touch_updated_at'";

/// Apply every migration not yet recorded in `cratestack_migrations`, then
/// (re-)install the `touch_updated_at` triggers `apply_pending` can't carry.
/// Returns the migration ids that were newly applied (empty if already
/// current) -- the trigger step isn't reflected in the return value, since
/// it isn't a tracked migration.
///
/// Concurrent calls are serialized with a session-scoped advisory lock so
/// the DDL steps cannot interleave with another process's inserts. The lock
/// lives on a dedicated connection that is killed (`close_on_drop`) at the
/// end of this call: if anything panics before the explicit unlock, the
/// session dies with the connection instead of leaking a held lock back
/// into the pool, where it would wedge every later caller that borrows that
/// connection.
pub async fn run(pool: &PgPool) -> Result<Vec<String>> {
    let mut lock_conn = pool
        .acquire()
        .await
        .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;
    lock_conn.close_on_drop();
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MIGRATE_LOCK_KEY)
        .execute(&mut *lock_conn)
        .await
        .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;

    let applied = apply_pending(pool, &migrations())
        .await
        .map_err(Error::Storage)?;
    let (installed,): (i64,) = sqlx::query_as(TRIGGERS_INSTALLED)
        .fetch_one(pool)
        .await
        .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;
    if installed < EXPECTED_TOUCH_TRIGGERS {
        sqlx::raw_sql(TOUCH_UPDATED_AT)
            .execute(pool)
            .await
            .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;
    }

    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATE_LOCK_KEY)
        .execute(&mut *lock_conn)
        .await
        .map_err(|e| Error::Storage(cratestack_error_from_sqlx(e)))?;
    Ok(applied)
}
