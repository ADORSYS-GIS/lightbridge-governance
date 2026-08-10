//! Shared domain for the governance platform: the tenant/application/integration
//! registry, credential issuance and the provider-agnostic normalized model that
//! every connector writes into (ADR-0005).
//!
//! Persistence is **cratestack only** (ADR-0009). `schema/governance.cstack` is
//! the source of truth for the tables, the migrations, the CRUD layer and the
//! REST routes; `include_server_schema!` expands it below. There is no
//! hand-written SQL in this workspace -- anything the generated CRUD cannot
//! express goes in a schema `procedure`, never in a second persistence path.
//!
//! Money is ALWAYS integer micro-USD (ADR-0008). There is no float in this crate.

pub mod connector_metrics;
pub mod credential;
pub mod error;
pub mod identity;
pub mod ingest;
pub mod migrate;
pub mod money;
pub mod org_kpis;
pub mod registry;

pub use error::{Error, Result};
pub use money::MicroUsd;

// Expands to the model structs, CRUD repositories, REST routes and migrations
// described by the schema. Kept in its own module so the generated items do not
// collide with the hand-written ones above.
pub mod schema {
    //! Generated from `schema/governance.cstack`. Do not edit -- change the
    //! schema and rebuild.
    cratestack::include_server_schema!("schema/governance.cstack", db = Postgres);
}
