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
//! 3. If the payload carries no identity, the token-derived identity is used
//!    and this is not an error.
//!
//! Note: Mismatch detection between token identity and payload email is not
//! implemented because the schema does not store email -> internal_user_id
//! mappings. The `identity_maps` table maps provider-specific user IDs (e.g.,
//! GitHub usernames) to internal user IDs, not emails.

use cratestack::{cool_error_from_sqlx, sqlx};
use sqlx::PgPool;

/// The result of resolving identity for a telemetry record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityResolution {
    /// The Keycloak sub (internal user ID) derived from the ingest token.
    /// This is the authoritative identity for attribution.
    pub internal_user_id: Option<String>,
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
