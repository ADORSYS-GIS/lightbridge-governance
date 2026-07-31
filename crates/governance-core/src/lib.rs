//! Shared domain for the governance platform: the tenant/application/integration
//! registry, credential issuance and the provider-agnostic normalized model that
//! every connector writes into (ADR-0005).
//!
//! Money is ALWAYS integer micro-USD (ADR-0008). There is no float in this crate.

pub mod error;
pub mod money;
pub mod registry;

pub use error::{Error, Result};
pub use money::MicroUsd;
