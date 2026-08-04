//! Push connector for OTLP-based AI telemetry (#30, RFC-0002).
//!
//! Generalized from the original Microsoft Foundry connector to support multiple
//! providers: Claude Code, Codex, and Foundry. Each provider emits OTLP spans with
//! slightly different attribute names, but all follow the same structure.
//!
//! This crate owns:
//! - The normalizer trait and per-provider implementations
//! - The dispatch logic that routes telemetry to the correct normalizer
//!
//! ⚠️ Never trust `tenant_id` from the telemetry body. It is derived from the
//! authenticated integration credential and stamped by Authorino.

pub mod normalizer;

/// Resource attributes Authorino stamps and the collector treats as trusted.
/// A producer that sets these itself must have them overwritten.
pub const TRUSTED_ATTRIBUTES: &[&str] = &[
    "governance.tenant.id",
    "governance.application.id",
    "governance.integration.id",
    "governance.environment",
    "governance.source",
];
