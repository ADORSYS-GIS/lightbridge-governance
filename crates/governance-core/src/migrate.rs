//! Applies the migrations cratestack derives from `schema/governance.cstack`
//! (ADR-0009). Generated with `cratestack migrate diff`; there is no
//! hand-written migration anywhere in this list -- add a new `Migration` entry
//! here each time the schema changes and a new migration directory is
//! generated under `migrations/postgres/`.

use cratestack::{Migration, apply_pending, sqlx::PgPool};

use crate::{Error, Result};

const INIT_UP: &str = include_str!("../migrations/postgres/20260802000939_init/up.sql");
const INIT_DOWN: &str = include_str!("../migrations/postgres/20260802000939_init/down.sql");

fn migrations() -> Vec<Migration> {
    vec![Migration {
        id: "20260802000939_init".to_owned(),
        description:
            "registry: tenants, applications, integrations, identity maps, ingest manifests"
                .to_owned(),
        up: INIT_UP.to_owned(),
        down: Some(INIT_DOWN.to_owned()),
    }]
}

/// Apply every migration not yet recorded in `cratestack_migrations`.
/// Returns the ids that were newly applied (empty if already current).
pub async fn run(pool: &PgPool) -> Result<Vec<String>> {
    apply_pending(pool, &migrations())
        .await
        .map_err(Error::Storage)
}
