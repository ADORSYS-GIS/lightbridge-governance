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
//!
//! ## Schema Assumption
//!
//! The `identity_maps` table stores provider-specific user identifiers in
//! `provider_user_id`. For Claude Code and Codex, this is the user's email
//! address. The table is populated by the identity mapping process (e.g., when
//! a user authenticates via GitHub OAuth, their GitHub email is mapped to their
//! Keycloak `internal_user_id`). This table must be populated by the identity
//! provider integration, not by the telemetry ingest pipeline.

use std::collections::{HashMap, HashSet};

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

/// Checks if payload emails mismatch the token-derived identity.
///
/// Returns a tuple of (token_user_id, mismatch_flags, query_failed) where:
/// - mismatch_flags[i] indicates whether payload_emails[i] mismatches the token identity
/// - query_failed indicates whether the identity_maps query failed (best-effort detection)
///
/// The mismatch check queries `identity_maps` for all emails in the batch at
/// once to avoid N+1 queries.
///
/// This is best-effort: if the query fails, all mismatch flags are false and
/// query_failed is true. The caller should increment a metric and log the error.
/// Mismatch detection should not block telemetry ingest.
pub async fn check_email_mismatches(
    pool: &PgPool,
    tenant_id: &str,
    provider: &str,
    token_user_id: Option<&str>,
    payload_emails: &[Option<&str>],
) -> (Option<String>, Vec<bool>, bool) {
    // Collect unique non-None emails for batch lookup.
    let unique_emails: Vec<&str> = payload_emails
        .iter()
        .filter_map(|e| *e)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    // Batch query identity_maps for all emails at once.
    let (email_mappings, query_failed) = if unique_emails.is_empty() {
        (HashMap::new(), false)
    } else {
        match sqlx::query_as::<_, (String, String)>(
            "SELECT provider_user_id, internal_user_id FROM identity_maps \
             WHERE tenant_id = $1 AND provider = $2 AND provider_user_id = ANY($3) \
             AND (valid_to IS NULL OR valid_to > now()) \
             ORDER BY valid_from DESC",
        )
        .bind(tenant_id)
        .bind(provider)
        .bind(&unique_emails)
        .fetch_all(pool)
        .await
        {
            Ok(rows) => {
                // Keep only the most recent mapping per email (first occurrence after ORDER BY).
                let mut map = HashMap::new();
                for (email, user_id) in rows {
                    map.entry(email).or_insert(user_id);
                }
                (map, false)
            }
            Err(e) => {
                // Best-effort: log error and continue without mismatch detection.
                tracing::error!(
                    tenant_id = %tenant_id,
                    provider = %provider,
                    error = %cool_error_from_sqlx(e),
                    "failed to query identity_maps for mismatch detection; continuing without mismatch alerts"
                );
                (HashMap::new(), true)
            }
        }
    };

    // Build mismatch flags for each email.
    let mismatches = payload_emails
        .iter()
        .map(|email_opt| {
            if let (Some(token_id), Some(email)) = (token_user_id, email_opt) {
                email_mappings
                    .get(*email)
                    .is_some_and(|mapped_id| mapped_id != token_id)
            } else {
                false
            }
        })
        .collect();

    (token_user_id.map(|s| s.to_string()), mismatches, query_failed)
}
