//! Identity resolution for telemetry attribution (#35).
//!
//! Identity comes from the ingest token, not from the payload. Both Claude Code
//! and Codex stamp `user.email` in their telemetry, but it is self-asserted by
//! the client and may be absent (API-key auth, metric-only records). The
//! credential is the thing we issued and can revoke; the email is a claim to
//! check against it.
//!
//! The resolution flow:
//! 1. The ingest token identifies an integration (via `/internal/v1/resolve`).
//! 2. The integration's bound `internal_user_id` (Keycloak sub) is the authoritative
//!    identity for attribution.
//! 3. If the payload's `user.email` disagrees with the token's bound identity,
//!    an alert fires but the token-derived identity wins (the mismatch is a
//!    signal worth seeing, not something to resolve silently).
//! 4. If the payload carries no identity, the token-derived identity is used
//!    and this is not an error.

use cratestack::{cool_error_from_sqlx, sqlx};
use sqlx::PgPool;

/// The result of resolving identity for a telemetry record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityResolution {
    /// The Keycloak sub (internal user ID) derived from the ingest token.
    /// This is the authoritative identity for attribution.
    pub internal_user_id: Option<String>,
    /// Whether there was a mismatch between the token-derived identity and the
    /// payload's email. This is a signal worth alerting on.
    pub mismatch: bool,
}

/// Looks up the integration's bound identity (token-derived).
///
/// This is the authoritative identity for attribution. Called once per batch
/// to avoid redundant queries.
pub async fn get_integration_identity(
    pool: &PgPool,
    tenant_id: &str,
    integration_id: &str,
) -> Result<Option<String>, crate::Error> {
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT internal_user_id FROM integrations \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(integration_id)
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| crate::Error::Storage(cool_error_from_sqlx(e)))?;

    Ok(row.and_then(|r| r.0))
}

/// Checks if a payload email mismatches the token-derived identity.
///
/// Returns an `IdentityResolution` with the token identity and mismatch status.
/// The mismatch check queries `identity_maps` for the given provider and email.
pub async fn check_email_mismatch(
    pool: &PgPool,
    tenant_id: &str,
    provider: &str,
    token_user_id: Option<&str>,
    payload_email: Option<&str>,
) -> Result<IdentityResolution, crate::Error> {
    let mismatch = if let (Some(token_id), Some(email)) = (token_user_id, payload_email) {
        // Look up what internal_user_id this email maps to for this provider.
        let email_mapping: Option<String> = sqlx::query_as(
            "SELECT internal_user_id FROM identity_maps \
             WHERE tenant_id = $1 AND provider = $2 AND provider_user_id = $3 \
             AND (valid_to IS NULL OR valid_to > now()) \
             ORDER BY valid_from DESC LIMIT 1",
        )
        .bind(tenant_id)
        .bind(provider)
        .bind(email)
        .fetch_optional(pool)
        .await
        .map_err(|e| crate::Error::Storage(cool_error_from_sqlx(e)))?
        .map(|(id,): (String,)| id);

        // Mismatch if the email maps to a different user than the token.
        email_mapping.is_some_and(|mapped_id| mapped_id != token_id)
    } else {
        false
    };

    Ok(IdentityResolution {
        internal_user_id: token_user_id.map(|s| s.to_string()),
        mismatch,
    })
}
