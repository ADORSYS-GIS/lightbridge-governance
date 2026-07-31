//! Microsoft Foundry connector (RFC-0002): a PUSH connector.
//!
//! Foundry hosted agents export OTLP to our public endpoint. This crate owns the
//! `/internal/v1/resolve` handler Authorino calls to turn an integration bearer
//! token into trusted tenant context, and the normalizer that turns GenAI spans
//! into execution / model-call / tool-call records.
//!
//! ⚠️ `resolve` sits in the ext_authz hot path of every customer request. It is
//! cached in-process (moka, 60s) -- so revocation propagates within one TTL, not
//! instantly. Say "within 60s" in customer docs rather than implying immediate.
//!
//! ⚠️ Never trust `tenant_id` from the telemetry body. It is derived from the
//! authenticated integration credential and stamped by Authorino.

/// Resource attributes Authorino stamps and the collector treats as trusted.
/// A producer that sets these itself must have them overwritten.
pub const TRUSTED_ATTRIBUTES: &[&str] = &[
    "governance.tenant.id",
    "governance.application.id",
    "governance.integration.id",
    "governance.environment",
    "governance.source",
];
